//! A single request boundary for supervisor-directed Provider attempts.
//!
//! This core service accepts explicit Provider / Model and BaseInput requests.
//! It does not select a target or expose Provider output and raw diagnostics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

static NEXT_PUBLICATION_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_CI_WAIT_OPERATION_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_VALIDATION_OPERATION_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_CONTEXT_CURSOR_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_CANCELLATION_OPERATION_ID: AtomicU64 = AtomicU64::new(0);

use crate::{
    Attempt, AttemptFailureReason, AttemptId, AttemptSemantics, AttemptState, CancellationToken,
    CiError, CiObservation, CiProvider, CiQueryTarget, CiRuntime, DomainError, LedgerError,
    ModelChoice, ModelRef, OperationId, ProviderError, ProviderRef, ProviderRequest,
    ProviderResolver, SqliteExecutionLedger, Task, TaskId, TaskRole, TaskState, UsageCost,
    UsageMetric, ValidationResult, Validator, WorkspaceError, WorkspaceManager,
    artifact::{
        ArtifactCodexDecisionRecord, ArtifactPublicationPermit, ArtifactValidationRecord,
        CodexDecisionKind,
    },
    artifact::{ArtifactError, ArtifactManager},
    artifact_publication::{
        ArtifactPublicationGateway, ArtifactPublicationPayload, DraftPullRequest, SecretScanResult,
        SecretScanner,
    },
    execution_ledger::{
        attempt_state_to_str, failure_reason_to_str, model_choice_kind, model_choice_name,
        task_state_to_str,
    },
};

/// Origin recorded in the immutable request snapshot for a newly created Task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskSource {
    Issue,
    Manual,
}

impl TaskSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Manual => "manual",
        }
    }
}

/// Issue details captured by the caller at task creation time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskIssueSnapshot {
    pub url: String,
    pub number: u64,
    pub title: String,
    pub body: String,
}

/// Input to the service-side `task.create` operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskCreateRequest {
    request_id: String,
    source: TaskSource,
    title: String,
    description: String,
    constraints: Vec<String>,
    issue: Option<TaskIssueSnapshot>,
}

impl TaskCreateRequest {
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        source: TaskSource,
        title: impl Into<String>,
        description: impl Into<String>,
        constraints: Vec<String>,
        issue: Option<TaskIssueSnapshot>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            source,
            title: title.into(),
            description: description.into(),
            constraints,
            issue,
        }
    }
}

/// The immutable request portion returned in a task.create snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskRequestSnapshot {
    source: TaskSource,
    title: String,
    description: String,
    constraints: Vec<String>,
    issue: Option<TaskIssueSnapshot>,
}

impl TaskRequestSnapshot {
    #[must_use]
    pub const fn source(&self) -> TaskSource {
        self.source
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
    #[must_use]
    pub fn constraints(&self) -> &[String] {
        &self.constraints
    }
    #[must_use]
    pub const fn issue(&self) -> Option<&TaskIssueSnapshot> {
        self.issue.as_ref()
    }
}

/// Durable result returned by `task.create`, including its original request snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskCreationResult {
    request_id: String,
    task_id: TaskId,
    revision: u64,
    state: TaskState,
    request: TaskRequestSnapshot,
}

/// Synchronous result for a Task completion request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskFinishResult {
    request_id: String,
    task_id: TaskId,
    revision: u64,
    artifact_id: String,
    evidence: Vec<(String, String)>,
}

/// One explicit validation command described by the #55 wire contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationCheckSpec {
    name: String,
    command: String,
    args: Vec<String>,
    timeout_ms: u64,
}

impl ValidationCheckSpec {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args,
            timeout_ms,
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn command(&self) -> &str {
        &self.command
    }
    pub fn args(&self) -> &[String] {
        &self.args
    }
    pub const fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
}

/// A configured validation policy resolves profiles and checks the command and
/// workspace against its trusted allowlist before running any command.
pub trait ValidationPolicy: Send + Sync {
    fn validate(
        &self,
        repository_root: &Path,
        workspace: &Path,
        profile_id: Option<&str>,
        checks: &[ValidationCheckSpec],
    ) -> Result<ValidationResult, crate::ValidatorError>;
}

struct PolicyValidator<'a> {
    policy: &'a dyn ValidationPolicy,
    repository_root: &'a Path,
    profile_id: Option<String>,
    checks: Vec<ValidationCheckSpec>,
}

impl Validator for PolicyValidator<'_> {
    fn validate(&self, workspace: &Path) -> Result<ValidationResult, crate::ValidatorError> {
        self.policy.validate(
            self.repository_root,
            workspace,
            self.profile_id.as_deref(),
            &self.checks,
        )
    }
}

fn validation_checks_json(
    ledger: &SqliteExecutionLedger,
    validation_id: &str,
) -> Result<Vec<serde_json::Value>, ServiceError> {
    let connection = ledger.lock_connection()?;
    let mut statement = connection.prepare("SELECT name,passed FROM artifact_validation_checks WHERE validation_id=?1 ORDER BY sequence")?;
    let rows = statement.query_map(params![validation_id], |row| {
        let name: String = row.get(0)?;
        let passed: bool = row.get(1)?;
        Ok(serde_json::json!({"name":name,"state":if passed {"passed"} else {"failed"},"diagnostic_ref":null}))
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(ServiceError::from)
}

fn validation_specs_json(checks: &[ValidationCheckSpec]) -> serde_json::Value {
    serde_json::Value::Array(checks.iter().map(|check| serde_json::json!({"name":check.name,"command":check.command,"args":check.args,"timeout_ms":check.timeout_ms})).collect())
}

fn validation_specs_from_json(
    value: serde_json::Value,
) -> Result<Vec<ValidationCheckSpec>, ServiceError> {
    value
        .as_array()
        .ok_or(ServiceError::InvalidStoredState)?
        .iter()
        .map(|check| {
            let name = check["name"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned();
            let command = check["command"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned();
            let args = check["args"]
                .as_array()
                .ok_or(ServiceError::InvalidStoredState)?
                .iter()
                .map(|arg| {
                    arg.as_str()
                        .map(ToOwned::to_owned)
                        .ok_or(ServiceError::InvalidStoredState)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let timeout_ms = check["timeout_ms"]
                .as_u64()
                .ok_or(ServiceError::InvalidStoredState)?;
            Ok(ValidationCheckSpec::new(name, command, args, timeout_ms))
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationRunRequest {
    request_id: String,
    task_id: TaskId,
    expected_revision: u64,
    artifact_id: String,
    profile_id: Option<String>,
    checks: Vec<ValidationCheckSpec>,
}

impl ValidationRunRequest {
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        task_id: TaskId,
        expected_revision: u64,
        artifact_id: impl Into<String>,
        profile_id: Option<String>,
        checks: Vec<ValidationCheckSpec>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            task_id,
            expected_revision,
            artifact_id: artifact_id.into(),
            profile_id,
            checks,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationAcceptance {
    operation_id: OperationId,
    task_id: TaskId,
    artifact_id: String,
    revision: u64,
    status: ServiceOperationStatus,
    accepted_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancellationAcceptance {
    operation_id: OperationId,
    task_id: TaskId,
    kind: &'static str,
    revision: u64,
    accepted_at_ms: i64,
}

impl CancellationAcceptance {
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn kind(&self) -> &'static str {
        self.kind
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn accepted_at_ms(&self) -> i64 {
        self.accepted_at_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CancellationOperationSnapshot {
    operation_id: OperationId,
    task_id: TaskId,
    kind: String,
    status: ServiceOperationStatus,
    revision: u64,
    accepted_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    result: Option<Value>,
    error_code: Option<String>,
}

impl CancellationOperationSnapshot {
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub const fn status(&self) -> ServiceOperationStatus {
        self.status
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn accepted_at_ms(&self) -> i64 {
        self.accepted_at_ms
    }
    pub const fn started_at_ms(&self) -> Option<i64> {
        self.started_at_ms
    }
    pub const fn finished_at_ms(&self) -> Option<i64> {
        self.finished_at_ms
    }
    pub fn result(&self) -> Option<&Value> {
        self.result.as_ref()
    }
    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }
}

impl ValidationAcceptance {
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn status(&self) -> ServiceOperationStatus {
        self.status
    }
    pub const fn accepted_at_ms(&self) -> i64 {
        self.accepted_at_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationOperationSnapshot {
    operation_id: OperationId,
    task_id: TaskId,
    artifact_id: String,
    status: ServiceOperationStatus,
    revision: u64,
    accepted_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    result: Option<serde_json::Value>,
    error_code: Option<String>,
}

impl ValidationOperationSnapshot {
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
    pub const fn status(&self) -> ServiceOperationStatus {
        self.status
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn accepted_at_ms(&self) -> i64 {
        self.accepted_at_ms
    }
    pub const fn started_at_ms(&self) -> Option<i64> {
        self.started_at_ms
    }
    pub const fn finished_at_ms(&self) -> Option<i64> {
        self.finished_at_ms
    }
    pub fn result(&self) -> Option<&serde_json::Value> {
        self.result.as_ref()
    }
    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }
}

impl TaskFinishResult {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
    pub fn evidence(&self) -> &[(String, String)] {
        &self.evidence
    }
}

impl TaskCreationResult {
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    #[must_use]
    pub const fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    #[must_use]
    pub const fn state(&self) -> TaskState {
        self.state
    }
    #[must_use]
    pub const fn source(&self) -> TaskSource {
        self.request.source
    }
    #[must_use]
    pub fn title(&self) -> &str {
        &self.request.title
    }
    #[must_use]
    pub fn description(&self) -> &str {
        &self.request.description
    }
    #[must_use]
    pub fn constraints(&self) -> &[String] {
        &self.request.constraints
    }
    #[must_use]
    pub const fn issue(&self) -> Option<&TaskIssueSnapshot> {
        self.request.issue.as_ref()
    }
    #[must_use]
    pub const fn request(&self) -> &TaskRequestSnapshot {
        &self.request
    }
}

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

/// A request to publish one exact Artifact using its saved Validation and Codex acceptance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPublicationRequest {
    request_id: String,
    task_id: TaskId,
    expected_revision: u64,
    artifact_id: String,
    validation_id: String,
    decision_id: String,
    payload: ArtifactPublicationPayload,
}

/// Stable identifier for one accepted Artifact Publication.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PublicationId(String);

impl PublicationId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Target for the synchronous CI Service preparation API.
///
/// A Publication target uses its stable `PublicationId`, never its operation ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CiServiceTarget {
    Publication(PublicationId),
    PullRequest { repository: String, number: u64 },
    Commit { repository: String, sha: String },
}

/// Immutable asynchronous `ci.wait` request. The enum enforces the exclusive
/// Publication ID versus direct-target choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiWaitRequest {
    request_id: String,
    task_id: TaskId,
    expected_revision: u64,
    target: CiServiceTarget,
    deadline: SystemTime,
}

impl CiWaitRequest {
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        task_id: TaskId,
        expected_revision: u64,
        target: CiServiceTarget,
        deadline: SystemTime,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            task_id,
            expected_revision,
            target,
            deadline,
        }
    }
}

impl ArtifactPublicationRequest {
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        task_id: TaskId,
        expected_revision: u64,
        artifact_id: impl Into<String>,
        validation_id: impl Into<String>,
        decision_id: impl Into<String>,
        payload: ArtifactPublicationPayload,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            task_id,
            expected_revision,
            artifact_id: artifact_id.into(),
            validation_id: validation_id.into(),
            decision_id: decision_id.into(),
            payload,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceOperationStatus {
    Accepted,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    RecoveryRequired,
}

/// Current outcome of a target-side cancellation request. `Running` means the
/// signal was accepted but the worker has not yet confirmed it stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CiWaitCancelTargetState {
    Cancelled,
    Running,
    RecoveryRequired,
}

struct ActiveCiWaitGuard<'a> {
    ledger: &'a SqliteExecutionLedger,
    operation_id: String,
    token: CancellationToken,
}

impl Drop for ActiveCiWaitGuard<'_> {
    fn drop(&mut self) {
        let _ = self
            .ledger
            .remove_ci_wait_cancellation(&self.operation_id, &self.token);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactPublicationPhase {
    Accepted,
    Scanned,
    Committing,
    Committed,
    Pushing,
    Pushed,
    CreatingDraft,
    Published,
    Failed,
    RecoveryRequired,
}

impl ArtifactPublicationPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Scanned => "scanned",
            Self::Committing => "committing",
            Self::Committed => "committed",
            Self::Pushing => "pushing",
            Self::Pushed => "pushed",
            Self::CreatingDraft => "creating_draft",
            Self::Published => "published",
            Self::Failed => "failed",
            Self::RecoveryRequired => "recovery_required",
        }
    }

    fn from_str(value: &str) -> Result<Self, ServiceError> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "scanned" => Ok(Self::Scanned),
            "committing" => Ok(Self::Committing),
            "committed" => Ok(Self::Committed),
            "pushing" => Ok(Self::Pushing),
            "pushed" => Ok(Self::Pushed),
            "creating_draft" => Ok(Self::CreatingDraft),
            "published" => Ok(Self::Published),
            "failed" => Ok(Self::Failed),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(ServiceError::InvalidStoredState),
        }
    }
}

/// Acceptance for `publication.publish`; it intentionally has no Attempt ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPublicationAcceptance {
    operation_id: OperationId,
    publication_id: PublicationId,
    request_id: String,
    task_id: TaskId,
    revision: u64,
    status: ServiceOperationStatus,
}

impl ArtifactPublicationAcceptance {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    #[must_use]
    pub fn publication_id(&self) -> &PublicationId {
        &self.publication_id
    }
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
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
pub struct ArtifactPublicationSnapshot {
    operation_id: OperationId,
    publication_id: PublicationId,
    request_id: String,
    task_id: TaskId,
    artifact_id: String,
    tree_oid: String,
    validation_id: String,
    decision_id: String,
    commit_sha: Option<String>,
    pull_request: Option<DraftPullRequest>,
    state: ServiceOperationStatus,
    phase: ArtifactPublicationPhase,
    revision: u64,
    error_code: Option<String>,
    accepted_at_ms: i64,
    finished_at_ms: Option<i64>,
}

impl ArtifactPublicationSnapshot {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    #[must_use]
    pub fn publication_id(&self) -> &PublicationId {
        &self.publication_id
    }
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    #[must_use]
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
    #[must_use]
    pub fn tree_oid(&self) -> &str {
        &self.tree_oid
    }
    #[must_use]
    pub fn validation_id(&self) -> &str {
        &self.validation_id
    }
    #[must_use]
    pub fn decision_id(&self) -> &str {
        &self.decision_id
    }
    #[must_use]
    pub fn commit_sha(&self) -> Option<&str> {
        self.commit_sha.as_deref()
    }
    #[must_use]
    pub fn pull_request(&self) -> Option<&DraftPullRequest> {
        self.pull_request.as_ref()
    }
    #[must_use]
    pub const fn state(&self) -> ServiceOperationStatus {
        self.state
    }
    #[must_use]
    pub const fn phase(&self) -> ArtifactPublicationPhase {
        self.phase
    }
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }
    #[must_use]
    pub const fn accepted_at_ms(&self) -> i64 {
        self.accepted_at_ms
    }
    #[must_use]
    pub const fn finished_at_ms(&self) -> Option<i64> {
        self.finished_at_ms
    }
}

impl ServiceOperationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
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
            "cancelling" => Ok(Self::Cancelling),
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

/// Acceptance for asynchronous `ci.wait`; it has no Attempt ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiWaitAcceptance {
    operation_id: OperationId,
    revision: u64,
    status: ServiceOperationStatus,
}

impl CiWaitAcceptance {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
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
pub struct CiWaitOperationSnapshot {
    operation_id: OperationId,
    task_id: TaskId,
    status: ServiceOperationStatus,
    observation: Option<CiObservation>,
    error_code: Option<String>,
    details_ref: Option<String>,
    revision: u64,
    accepted_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationGetResult {
    Attempt(OperationSnapshot),
    CiWait(CiWaitOperationSnapshot),
    ArtifactPublication(ArtifactPublicationSnapshot),
    Validation(ValidationOperationSnapshot),
    Cancellation(CancellationOperationSnapshot),
}

impl CiWaitOperationSnapshot {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    #[must_use]
    pub const fn status(&self) -> ServiceOperationStatus {
        self.status
    }
    #[must_use]
    pub const fn observation(&self) -> Option<&CiObservation> {
        self.observation.as_ref()
    }
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }
    #[must_use]
    pub fn details_ref(&self) -> Option<&str> {
        self.details_ref.as_deref()
    }
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
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
    Forbidden(&'static str),
    IdempotencyConflict,
    EvidenceArtifactMismatch,
    OperationNotFound,
    InvalidStoredState,
    InvalidCursor,
    NotCancellable,
    ValidationFailed,
    PublicationFailed,
    PublicationRecoveryRequired,
    CiProviderUnavailable,
    Ci(CiError),
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
            Self::Forbidden(reason) => write!(formatter, "operation is forbidden: {reason}"),
            Self::IdempotencyConflict => {
                formatter.write_str("request ID was already used with a different payload")
            }
            Self::EvidenceArtifactMismatch => {
                formatter.write_str("evidence does not belong to the requested Artifact")
            }
            Self::OperationNotFound => formatter.write_str("operation was not found"),
            Self::InvalidCursor => {
                formatter.write_str("context cursor is invalid for this Task snapshot")
            }
            Self::NotCancellable => formatter
                .write_str("the requested operation cannot be cancelled in its current state"),
            Self::InvalidStoredState => {
                formatter.write_str("invalid stored Operation Service state")
            }
            Self::ValidationFailed => {
                formatter.write_str("Artifact validation could not be completed safely")
            }
            Self::PublicationFailed => {
                formatter.write_str("Artifact publication failed before remote effects")
            }
            Self::PublicationRecoveryRequired => {
                formatter.write_str("Artifact publication requires recovery; it was not replayed")
            }
            Self::CiProviderUnavailable => formatter.write_str("CI Provider is not configured"),
            Self::Ci(error) => error.fmt(formatter),
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

impl From<CiError> for ServiceError {
    fn from(error: CiError) -> Self {
        Self::Ci(error)
    }
}

fn validate_task_create(request: &TaskCreateRequest) -> Result<(), ServiceError> {
    match (request.source, request.issue.as_ref()) {
        (TaskSource::Issue, None) => {
            return Err(ServiceError::InvalidRequest(
                "issue source requires an issue snapshot",
            ));
        }
        (TaskSource::Manual, Some(_)) => {
            return Err(ServiceError::InvalidRequest(
                "manual source must omit the issue snapshot",
            ));
        }
        _ => {}
    }
    if let Some(issue) = &request.issue {
        if !is_absolute_uri(&issue.url) || issue.number == 0 {
            return Err(ServiceError::InvalidRequest(
                "issue snapshot has an invalid URI or issue number",
            ));
        }
    }
    Ok(())
}

fn repository_from_pull_request_url(url: &str, expected_number: u64) -> Option<String> {
    let remainder = url.strip_prefix("https://")?;
    let (host, path) = remainder.split_once('/')?;
    if host != "github.com" {
        return None;
    }
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.len() != 4
        || segments[2] != "pull"
        || segments[3] != expected_number.to_string()
        || !valid_repository_component(segments[0])
        || !valid_repository_component(segments[1])
    {
        return None;
    }
    Some(format!("{}/{}", segments[0], segments[1]))
}

fn valid_repository_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_absolute_uri(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once(':') else {
        return false;
    };
    if scheme.is_empty()
        || !scheme.chars().enumerate().all(|(index, character)| {
            character.is_ascii_alphabetic()
                || (index > 0
                    && (character.is_ascii_digit() || matches!(character, '+' | '-' | '.')))
        })
    {
        return false;
    }
    let bytes = rest.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
            continue;
        }
        let valid = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b':'
                    | b'/'
                    | b'?'
                    | b'#'
                    | b'['
                    | b']'
                    | b'@'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
            );
        if !valid {
            return false;
        }
        index += 1;
    }
    true
}

fn context_timestamp(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60,
        ms.rem_euclid(1000)
    )
}

fn context_item(
    id: &str,
    kind: &str,
    state: Option<&str>,
    at: i64,
    summary: &str,
    references: Value,
    details: Value,
) -> Value {
    serde_json::json!({"id":id,"kind":kind,"state":state,"occurred_at":context_timestamp(at),"summary":summary,"references":references,"details":details,"_at":at})
}

fn context_items(
    connection: &rusqlite::Connection,
    task_id: &TaskId,
    section: &str,
) -> Result<Vec<Value>, ServiceError> {
    let mut items = Vec::new();
    match section {
        "providers" => {
            let mut stmt = connection.prepare("SELECT observed_provider,observed_model,finished_at FROM service_operations WHERE task_id=?1 AND observed_provider IS NOT NULL AND finished_at IS NOT NULL ORDER BY finished_at DESC,id DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut seen = std::collections::BTreeSet::new();
            for row in rows {
                let (provider, model, at) = row?;
                if !seen.insert(provider.clone()) {
                    continue;
                }
                let models = model.into_iter().collect::<Vec<_>>();
                items.push(context_item(&provider,"provider",Some("unknown"),at,"Provider availability is unknown",serde_json::json!([]),serde_json::json!({"provider_id":provider,"model_ids":models,"availability":"unknown","observed_at":context_timestamp(at),"diagnostic_ref":null})));
            }
        }
        "attempts" => {
            let mut stmt=connection.prepare("SELECT o.id,o.attempt_id,o.provider,o.model_kind,o.model_name,o.observed_provider,o.observed_model,o.role,o.base_commit,o.status,o.accepted_at,r.input_artifact_id,r.output_artifact_id FROM service_operations o LEFT JOIN service_attempt_artifacts r ON r.task_id=o.task_id AND r.attempt_id=o.attempt_id WHERE o.task_id=?1 ORDER BY o.accepted_at DESC,o.id DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, i64>(10)?,
                    r.get::<_, Option<String>>(11)?,
                    r.get::<_, Option<String>>(12)?,
                ))
            })?;
            for row in rows {
                let (id, attempt, provider, mk, mn, op, om, role, base, status, at, input, output) =
                    row?;
                let state = match status.as_str() {
                    "completed" => "succeeded",
                    "running" => "running",
                    "accepted" => "queued",
                    "cancelled" => "cancelled",
                    _ => "failed",
                };
                let model = if mk == "named" {
                    serde_json::json!({"kind":"named","model":mn})
                } else {
                    serde_json::json!({"kind":"provider_default"})
                };
                let refs = output
                    .as_ref()
                    .map(|v| serde_json::json!([{"kind":"artifact","id":v}]))
                    .unwrap_or(serde_json::json!([]));
                items.push(context_item(&attempt,"attempt",Some(state),at,"Provider attempt",refs,serde_json::json!({"requested_provider_id":provider,"requested_model":model,"observed_provider_id":op,"observed_model_id":om,"role":role,"input_artifact_id":input,"base_commit":base,"output_artifact_id":output,"diagnostic_ref":null})));
                let _ = id;
            }
        }
        "artifacts" => {
            let mut stmt=connection.prepare("SELECT id,source_attempt_id,base_commit,tree_oid,state,created_at FROM service_artifacts WHERE task_id=?1 ORDER BY created_at DESC,id DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            for row in rows {
                let (id, attempt, base, digest, state, at) = row?;
                items.push(context_item(&id,"artifact",None,at,"Saved artifact",serde_json::json!([]),serde_json::json!({"artifact_id":id,"digest":digest,"source_attempt_id":attempt,"base_commit":base,"diff_ref":null})));
                let _ = state;
            }
        }
        "validations" => {
            let mut stmt=connection.prepare("SELECT v.id,v.artifact_id,v.passed,v.summary,v.created_at,(SELECT profile_id FROM service_validation_operations WHERE validation_id=v.id ORDER BY accepted_at DESC LIMIT 1) FROM artifact_validations v WHERE v.task_id=?1 ORDER BY v.created_at DESC,v.id DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?;
            for row in rows {
                let (id, artifact, passed, summary, at, profile_id) = row?;
                let mut checks = Vec::new();
                let mut check_stmt=connection.prepare("SELECT name,passed FROM artifact_validation_checks WHERE validation_id=?1 ORDER BY sequence")?;
                let check_rows = check_stmt.query_map(params![id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
                })?;
                for check in check_rows {
                    let (n, p) = check?;
                    checks.push(serde_json::json!({"name":n,"state":if p{"passed"}else{"failed"},"diagnostic_ref":null}));
                }
                items.push(context_item(&id,"validation",Some(if passed{"passed"}else{"failed"}),at,&summary,serde_json::json!([{"kind":"artifact","id":artifact}]),serde_json::json!({"validation_id":id,"artifact_id":artifact,"check_profile_id":profile_id,"checks":checks})));
            }
        }
        "decisions" => {
            let mut stmt=connection.prepare("SELECT id,artifact_id,decision,reason,evidence_json,created_at FROM artifact_codex_decisions WHERE task_id=?1 ORDER BY created_at DESC,id DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            for row in rows {
                let (id, artifact, decision, reason, evidence, at) = row?;
                let pairs: Vec<(String, String)> = serde_json::from_str(&evidence)
                    .map_err(|_| ServiceError::InvalidStoredState)?;
                let refs: Vec<Value> = pairs
                    .iter()
                    .map(|(kind, id)| serde_json::json!({"kind":kind,"id":id}))
                    .collect();
                let parsed: Vec<Value> = refs.clone();
                items.push(context_item(&id,"codex_decision",Some(&decision),at,&reason,serde_json::json!([{"kind":"artifact","id":artifact}]),serde_json::json!({"decision_id":id,"artifact_id":artifact,"decision":decision,"reason":reason,"evidence":parsed})));
            }
        }
        "publication" => {
            let mut stmt=connection.prepare("SELECT identity.publication_id,publication.artifact_id,publication.commit_sha,publication.pull_request_number,publication.pull_request_url,publication.accepted_at FROM service_artifact_publication_operations publication JOIN service_publication_ids identity ON identity.operation_id=publication.id WHERE publication.task_id=?1 AND publication.status='completed' AND publication.commit_sha IS NOT NULL AND publication.pull_request_number IS NOT NULL AND publication.pull_request_url IS NOT NULL ORDER BY publication.accepted_at DESC,identity.publication_id DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            for row in rows {
                let (id, artifact, sha, number, url, at) = row?;
                let repository = repository_from_pull_request_url(
                    &url,
                    u64::try_from(number).map_err(|_| ServiceError::InvalidStoredState)?,
                )
                .ok_or(ServiceError::InvalidStoredState)?;
                items.push(context_item(&id,"publication",None,at,"Artifact publication",serde_json::json!([{"kind":"artifact","id":artifact}]),serde_json::json!({"publication_id":id,"artifact_id":artifact,"repository":repository,"head_sha":sha,"pull_request_number":number,"pull_request_url":url})));
            }
        }
        "ci" => {
            let mut stmt=connection.prepare("SELECT id,repository,pull_request_number,head_sha,state,observed_at_ms FROM ci_observations WHERE task_id=?1 ORDER BY observed_at_ms DESC,id DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            for row in rows {
                let (id, repo, number, sha, state, at) = row?;
                let mut checks = Vec::new();
                let mut cstmt=connection.prepare("SELECT name,state,url,completed_at FROM ci_observation_checks WHERE observation_id=?1 ORDER BY sequence")?;
                let cr = cstmt.query_map(params![id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?;
                for c in cr {
                    let (n, s, u, t) = c?;
                    checks.push(serde_json::json!({"name":n,"state":s,"url":u,"completed_at":t}));
                }
                items.push(context_item(&id,"ci_observation",Some(&state),at,"CI observation",serde_json::json!([]),serde_json::json!({"observation_id":id,"repository":repo,"pull_request_number":number,"head_sha":sha,"state":state,"checks":checks})));
            }
        }
        "usage" => {
            let mut stmt=connection.prepare("SELECT u.operation_id,u.sequence,u.name,u.value,u.unit,COALESCE(o.finished_at,o.accepted_at) FROM service_operation_usage u JOIN service_operations o ON o.id=u.operation_id WHERE o.task_id=?1 ORDER BY COALESCE(o.finished_at,o.accepted_at) DESC,u.sequence DESC")?;
            let rows = stmt.query_map(params![task_id.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            for row in rows {
                let (op, sequence, name, value, unit, at) = row?;
                let value = value
                    .parse::<f64>()
                    .map_err(|_| ServiceError::InvalidStoredState)?;
                items.push(context_item(&format!("{op}:{sequence}"),"usage",None,at,"Recorded usage metric",serde_json::json!([{"kind":"operation","id":op}]),serde_json::json!({"name":name,"value":value,"unit":unit,"basis":"unknown","observed_at":context_timestamp(at)})));
            }
        }
        "reviews" => {}
        _ => return Err(ServiceError::InvalidRequest("unknown context section")),
    }
    Ok(items)
}

fn task_request_json(request: &TaskCreateRequest) -> String {
    let issue = request.issue.as_ref().map(|issue| {
        serde_json::json!({
            "url": issue.url, "number": issue.number, "title": issue.title, "body": issue.body
        })
    });
    serde_json::json!({
        "source": request.source.as_str(), "title": request.title,
        "description": request.description, "constraints": request.constraints,
        "issue": issue
    })
    .to_string()
}

fn load_task_creation_result(
    connection: &rusqlite::Connection,
    request_id: &str,
    task_id: &str,
) -> Result<TaskCreationResult, ServiceError> {
    let json: String = connection.query_row(
        "SELECT request_json FROM task_request_snapshots WHERE task_id=?1",
        params![task_id],
        |row| row.get(0),
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|_| ServiceError::InvalidStoredState)?;
    let source = match value["source"].as_str() {
        Some("issue") => TaskSource::Issue,
        Some("manual") => TaskSource::Manual,
        _ => return Err(ServiceError::InvalidStoredState),
    };
    let issue = if value["issue"].is_null() {
        None
    } else {
        Some(TaskIssueSnapshot {
            url: value["issue"]["url"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
            number: value["issue"]["number"]
                .as_u64()
                .ok_or(ServiceError::InvalidStoredState)?,
            title: value["issue"]["title"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
            body: value["issue"]["body"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
        })
    };
    let constraints = value["constraints"]
        .as_array()
        .ok_or(ServiceError::InvalidStoredState)?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or(ServiceError::InvalidStoredState)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TaskCreationResult {
        request_id: request_id.to_owned(),
        task_id: TaskId::new(task_id),
        revision: 0,
        state: TaskState::Pending,
        request: TaskRequestSnapshot {
            source,
            title: value["title"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
            description: value["description"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
            constraints,
            issue,
        },
    })
}

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
        match error {
            ArtifactError::IdempotencyConflict => Self::IdempotencyConflict,
            ArtifactError::TaskNotFound => Self::TaskNotFound,
            ArtifactError::StaleRevision { expected, actual } => {
                Self::StaleRevision { expected, actual }
            }
            other => Self::Artifact(other),
        }
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
    secret_scanner: Option<&'a dyn SecretScanner>,
    publication_gateway: Option<&'a dyn ArtifactPublicationGateway>,
    run_lock: Option<crate::operation_ledger::LedgerRunLock>,
    ci_provider: Option<&'a (dyn CiProvider + Sync)>,
    ci_poll_interval: Duration,
    ci_api_timeout: Duration,
    validation_policy: Option<&'a dyn ValidationPolicy>,
    active_cancel_tokens: std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
}

struct ActiveCancellationGuard<'a> {
    registry: &'a std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>,
    operation_id: String,
}

impl Drop for ActiveCancellationGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.registry.lock() {
            active.remove(&self.operation_id);
        }
    }
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
            secret_scanner: None,
            publication_gateway: None,
            run_lock,
            ci_provider: None,
            ci_poll_interval: Duration::from_millis(250),
            ci_api_timeout: default_timeout,
            validation_policy: None,
            active_cancel_tokens: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        service.recover_incomplete_operations()?;
        if ledger.ledger_path().is_some() {
            ArtifactManager::new(workspaces, ledger).recover_pending()?;
        }
        Ok(service)
    }

    /// Injects the caller's secret scanning policy. Publication is denied when absent.
    #[must_use]
    pub fn with_secret_scanner(mut self, scanner: &'a dyn SecretScanner) -> Self {
        self.secret_scanner = Some(scanner);
        self
    }

    /// Injects the Git/GitHub publication adapter used after both scans pass.
    #[must_use]
    pub fn with_artifact_publication_gateway(
        mut self,
        gateway: &'a dyn ArtifactPublicationGateway,
    ) -> Self {
        self.publication_gateway = Some(gateway);
        self
    }

    /// Injects the read-only GitHub CI observer used by the synchronous
    /// preparation Service API. Full asynchronous `ci.wait` acceptance is a
    /// separate wire-contract integration.
    pub fn with_ci_provider(
        mut self,
        provider: &'a (dyn CiProvider + Sync),
        poll_interval: Duration,
        api_timeout: Duration,
    ) -> Result<Self, ServiceError> {
        if poll_interval.is_zero() || api_timeout.is_zero() {
            return Err(ServiceError::InvalidRequest(
                "CI poll interval and API timeout must be positive",
            ));
        }
        self.ci_provider = Some(provider);
        self.ci_poll_interval = poll_interval;
        self.ci_api_timeout = api_timeout;
        Ok(self)
    }

    /// Injects trusted validation profiles and an explicit command/workspace allowlist.
    #[must_use]
    pub fn with_validation_policy(mut self, policy: &'a dyn ValidationPolicy) -> Self {
        self.validation_policy = Some(policy);
        self
    }

    /// Creates a pending Task and its revision zero snapshot atomically. The
    /// caller scopes the task.create request ID; replay returns the first result.
    pub fn create_task(
        &self,
        caller: &str,
        request: &TaskCreateRequest,
    ) -> Result<TaskCreationResult, ServiceError> {
        if caller.is_empty() {
            return Err(ServiceError::InvalidRequest(
                "caller identity must not be empty",
            ));
        }
        validate_task_create(request)?;
        let payload = task_request_json(request);
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT request_json, task_id FROM task_create_idempotency
             WHERE caller=?1 AND tool_name='task.create' AND request_id=?2",
                params![caller, request.request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((stored, task_id)) = existing {
            if stored != payload {
                return Err(ServiceError::IdempotencyConflict);
            }
            let result = load_task_creation_result(&tx, &request.request_id, &task_id)?;
            tx.commit()?;
            return Ok(result);
        }

        tx.execute("INSERT INTO service_task_id_sequence DEFAULT VALUES", [])?;
        let sequence = tx.last_insert_rowid();
        let mut task_id = TaskId::new(format!("task-create-{sequence}"));
        while tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
            params![task_id.as_str()],
            |row| row.get::<_, bool>(0),
        )? {
            tx.execute("INSERT INTO service_task_id_sequence DEFAULT VALUES", [])?;
            task_id = TaskId::new(format!("task-create-{}", tx.last_insert_rowid()));
        }
        tx.execute(
            "INSERT INTO tasks(id,description,role,state) VALUES(?1,?2,'unspecified','pending')",
            params![task_id.as_str(), request.description],
        )?;
        tx.execute(
            "INSERT INTO service_task_revisions(task_id,revision) VALUES(?1,0)",
            params![task_id.as_str()],
        )?;
        tx.execute(
            "INSERT INTO task_request_snapshots(task_id,request_json) VALUES(?1,?2)",
            params![task_id.as_str(), payload],
        )?;
        tx.execute(
            "INSERT INTO task_create_idempotency(caller,tool_name,request_id,request_json,task_id)
             VALUES(?1,'task.create',?2,?3,?4)",
            params![caller, request.request_id, payload, task_id.as_str()],
        )?;
        let result = TaskCreationResult {
            request_id: request.request_id.clone(),
            task_id,
            revision: 0,
            state: TaskState::Pending,
            request: TaskRequestSnapshot {
                source: request.source,
                title: request.title.clone(),
                description: request.description.clone(),
                constraints: request.constraints.clone(),
                issue: request.issue.clone(),
            },
        };
        tx.commit()?;
        Ok(result)
    }

    /// Completes a Task only after checking the exact available Artifact and its
    /// latest accepted decision. The state transition, revision, and durable
    /// caller-scoped response are committed atomically.
    #[allow(clippy::too_many_arguments)] // Parameters are the atomic Task.finish contract fields.
    pub fn finish_task_idempotent(
        &self,
        caller: &str,
        request_id: &str,
        task_id: &TaskId,
        expected_revision: u64,
        artifact_id: &str,
        decision_id: &str,
        evidence: &[(String, String)],
    ) -> Result<TaskFinishResult, ServiceError> {
        if caller.trim().is_empty()
            || request_id.trim().is_empty()
            || artifact_id.trim().is_empty()
            || decision_id.trim().is_empty()
        {
            return Err(ServiceError::InvalidRequest(
                "caller, request, Artifact, and decision IDs must not be empty",
            ));
        }
        let mut normalized_evidence = evidence.to_vec();
        normalized_evidence.sort();
        let request_json = serde_json::to_string(&(
            task_id.as_str(),
            expected_revision,
            artifact_id,
            decision_id,
            &normalized_evidence,
        ))
        .map_err(|_| ServiceError::InvalidStoredState)?;
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replay: Option<(String, String)> = tx.query_row(
            "SELECT request_json,response_json FROM service_mcp_idempotency WHERE caller=?1 AND tool_name='task.finish' AND request_id=?2",
            params![caller, request_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        if let Some((stored, response)) = replay {
            if stored != request_json {
                return Err(ServiceError::IdempotencyConflict);
            }
            let value: serde_json::Value =
                serde_json::from_str(&response).map_err(|_| ServiceError::InvalidStoredState)?;
            let evidence = serde_json::from_value(value["evidence"].clone())
                .map_err(|_| ServiceError::InvalidStoredState)?;
            let result = TaskFinishResult {
                request_id: request_id.to_owned(),
                task_id: TaskId::new(task_id.as_str()),
                revision: value["revision"]
                    .as_u64()
                    .ok_or(ServiceError::InvalidStoredState)?,
                artifact_id: value["artifact_id"]
                    .as_str()
                    .ok_or(ServiceError::InvalidStoredState)?
                    .to_owned(),
                evidence,
            };
            tx.commit()?;
            return Ok(result);
        }
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let revision = revision.ok_or(ServiceError::TaskNotFound)?;
        if u64::try_from(revision).ok() != Some(expected_revision) {
            return Err(ServiceError::StaleRevision {
                expected: expected_revision,
                actual: u64::try_from(revision).unwrap_or_default(),
            });
        }
        let artifact_tree: Option<String> = tx.query_row(
            "SELECT tree_oid FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'",
            params![task_id.as_str(), artifact_id], |row| row.get(0),
        ).optional()?;
        let artifact_tree = artifact_tree.ok_or(ServiceError::Artifact(ArtifactError::NotFound))?;
        let saved_decision: Option<(String, String)> = tx.query_row(
            "SELECT tree_oid,evidence_json FROM artifact_codex_decisions WHERE id=?1 AND task_id=?2 AND artifact_id=?3 AND decision='accepted' AND rowid=(SELECT MAX(rowid) FROM artifact_codex_decisions WHERE task_id=?2 AND artifact_id=?3)",
            params![decision_id, task_id.as_str(), artifact_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (decision_tree, _) = saved_decision.ok_or(ServiceError::PolicyDenied(
            "Task finish requires the latest accepted decision for the requested Artifact",
        ))?;
        if decision_tree != artifact_tree {
            return Err(ServiceError::EvidenceArtifactMismatch);
        }
        let mut publication_target: Option<(String, Option<u64>, String)> = None;
        // Resolve Publication refs first so CI refs work regardless of input
        // ordering (evidence arrays are semantically a set of references).
        for (kind, id) in &normalized_evidence {
            if kind == "publication" {
                let target: Option<(String, Option<i64>, String)> = tx.query_row(
                    "SELECT publication.commit_sha,publication.pull_request_number,publication.pull_request_url
                     FROM service_publication_ids identity JOIN service_artifact_publication_operations publication
                       ON publication.id=identity.operation_id
                     WHERE identity.publication_id=?1 AND publication.task_id=?2 AND publication.artifact_id=?3",
                    params![id, task_id.as_str(), artifact_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                ).optional()?;
                let (sha, number, url) = target.ok_or(ServiceError::EvidenceArtifactMismatch)?;
                let number = number.ok_or(ServiceError::InvalidStoredState)?;
                let number = u64::try_from(number).map_err(|_| ServiceError::InvalidStoredState)?;
                let repository = repository_from_pull_request_url(&url, number)
                    .ok_or(ServiceError::InvalidStoredState)?;
                publication_target = Some((repository, Some(number), sha));
            }
        }
        for (kind, id) in &normalized_evidence {
            match kind.as_str() {
                "validation" => {
                    let matches: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM artifact_validations WHERE id=?1 AND task_id=?2 AND artifact_id=?3 AND tree_oid=?4)",
                        params![id, task_id.as_str(), artifact_id, artifact_tree], |row| row.get(0),
                    )?;
                    if !matches {
                        return Err(ServiceError::EvidenceArtifactMismatch);
                    }
                }
                "decision" => {
                    let matches: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM artifact_codex_decisions WHERE id=?1 AND task_id=?2 AND artifact_id=?3 AND tree_oid=?4)",
                        params![id, task_id.as_str(), artifact_id, artifact_tree], |row| row.get(0),
                    )?;
                    if !matches {
                        return Err(ServiceError::EvidenceArtifactMismatch);
                    }
                }
                "publication" => {}
                "ci" => {
                    let observation: Option<(Option<String>, String, Option<i64>, String)> = tx.query_row(
                        "SELECT task_id,repository,pull_request_number,head_sha FROM ci_observations WHERE id=?1",
                        params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    ).optional()?;
                    let (observed_task, repository, number, sha) = observation
                        .ok_or(ServiceError::InvalidRequest("CI evidence ID was not found"))?;
                    if observed_task
                        .as_deref()
                        .is_some_and(|observed| observed != task_id.as_str())
                    {
                        return Err(ServiceError::EvidenceArtifactMismatch);
                    }
                    let Some((published_repository, published_number, published_sha)) =
                        &publication_target
                    else {
                        return Err(ServiceError::EvidenceArtifactMismatch);
                    };
                    if &repository != published_repository
                        || number.and_then(|value| u64::try_from(value).ok()) != *published_number
                        || &sha != published_sha
                    {
                        return Err(ServiceError::EvidenceArtifactMismatch);
                    }
                }
                // Review records are not persisted by this Service yet. A typed
                // policy rejection is safer than accepting an unverifiable ref.
                "review" => {
                    return Err(ServiceError::PolicyDenied(
                        "review verdict evidence is not configured",
                    ));
                }
                _ => return Err(ServiceError::InvalidRequest("unsupported evidence kind")),
            }
        }
        let (description, role, state): (String, String, String) = tx.query_row(
            "SELECT description,role,state FROM tasks WHERE id=?1",
            params![task_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let state = match state.as_str() {
            "pending" => TaskState::Pending,
            "active" => TaskState::Active,
            "completed" => TaskState::Completed,
            "failed" => TaskState::Failed,
            "cancelled" => TaskState::Cancelled,
            _ => return Err(ServiceError::InvalidStoredState),
        };
        let mut task = Task::restore(
            task_id.clone(),
            description,
            TaskRole::new(role),
            state,
            Vec::new(),
        );
        task.complete()?;
        let next_revision = expected_revision
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or(ServiceError::InvalidStoredState)?;
        tx.execute(
            "UPDATE tasks SET state=?2 WHERE id=?1",
            params![task_id.as_str(), task_state_to_str(task.state())],
        )?;
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![task_id.as_str(), next_revision as i64],
        )?;
        let result = TaskFinishResult {
            request_id: request_id.to_owned(),
            task_id: task_id.clone(),
            revision: next_revision,
            artifact_id: artifact_id.to_owned(),
            evidence: normalized_evidence,
        };
        let response = serde_json::json!({"revision":result.revision,"artifact_id":result.artifact_id,"evidence":result.evidence}).to_string();
        tx.execute("INSERT INTO service_mcp_idempotency(caller,tool_name,request_id,request_json,response_json) VALUES(?1,'task.finish',?2,?3,?4)", params![caller, request_id, request_json, response])?;
        tx.commit()?;
        Ok(result)
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
        let active_publication: Option<String> = transaction.query_row(
            "SELECT id FROM service_artifact_publication_operations WHERE task_id=?1 AND status IN ('accepted','running','recovery_required') ORDER BY rowid LIMIT 1",
            params![request.task_id.as_str()],
            |row| row.get(0),
        ).optional()?;
        if let Some(id) = active_publication {
            return Err(ServiceError::Busy(OperationId::new(id)));
        }

        if let Some(artifact_id) = input_artifact_id.as_deref() {
            let stored_artifact: Option<(String, String)> = transaction
                .query_row(
                    "SELECT base_commit,repository_root FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'",
                    params![request.task_id.as_str(), artifact_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let (stored_base, stored_repository) =
                stored_artifact.ok_or(ServiceError::Artifact(ArtifactError::NotFound))?;
            if stored_base != base_commit
                || std::path::PathBuf::from(stored_repository) != repository
            {
                return Err(ServiceError::Artifact(ArtifactError::Invalid(
                    "artifact input changed during request validation".into(),
                )));
            }
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
        self.active_cancel_tokens
            .lock()
            .map_err(|_| ServiceError::InvalidStoredState)?
            .insert(operation_id.as_str().to_owned(), cancellation.clone());
        let _active_guard = ActiveCancellationGuard {
            registry: &self.active_cancel_tokens,
            operation_id: operation_id.as_str().to_owned(),
        };
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

    /// Runs the configured validator on a fresh worktree restored from the immutable Artifact.
    /// A result is recorded only if the same Git tree is present before and after validation.
    pub fn validate_artifact(
        &self,
        task_id: &TaskId,
        artifact_id: &str,
        expected_revision: u64,
        validator: &dyn Validator,
    ) -> Result<ArtifactValidationRecord, ServiceError> {
        static NEXT_VALIDATION: AtomicU64 = AtomicU64::new(0);
        let artifacts = ArtifactManager::new(self.workspaces, self.ledger);
        artifacts.check_revision(task_id, expected_revision)?;
        let artifact = artifacts.verify_input(task_id, artifact_id)?;
        let attempt_id = AttemptId::new(format!(
            "validation-{}-{}",
            now_ms(),
            NEXT_VALIDATION.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace =
            self.workspaces
                .create_at_base(task_id, &attempt_id, artifact.base_commit())?;
        let initial_tree = match artifacts.snapshot_workspace(workspace.path()) {
            Ok(tree) => tree,
            Err(error) => {
                return Err(ServiceError::Artifact(
                    ArtifactManager::retained_workspace_error(&workspace, error.to_string()),
                ));
            }
        };
        if let Err(error) = artifacts.materialize(&workspace, task_id, &attempt_id, artifact_id) {
            return match artifacts.cleanup_validation_workspace(
                task_id,
                &attempt_id,
                &workspace,
                &initial_tree,
            ) {
                Ok(()) => Err(ServiceError::from(error)),
                Err(cleanup_error) => Err(ServiceError::Artifact(
                    ArtifactManager::retained_workspace_error(
                        &workspace,
                        format!("materialization failed ({error}); {cleanup_error}"),
                    ),
                )),
            };
        }
        if let Err(error) = artifacts.verify_workspace_tree(workspace.path(), artifact.tree_oid()) {
            return Err(ServiceError::Artifact(
                ArtifactManager::retained_workspace_error(&workspace, error.to_string()),
            ));
        }
        let result: ValidationResult = match validator.validate(workspace.path()) {
            Ok(result) => result,
            Err(error) => {
                return match artifacts.cleanup_unchanged_artifact_validation_workspace(
                    task_id,
                    &attempt_id,
                    artifact_id,
                    &workspace,
                ) {
                    Ok(()) => Err(ServiceError::ValidationFailed),
                    Err(cleanup_error) => Err(ServiceError::Artifact(
                        ArtifactManager::retained_workspace_error(
                            &workspace,
                            format!("validation failed ({error}); {cleanup_error}"),
                        ),
                    )),
                };
            }
        };
        if let Err(error) = artifacts.verify_workspace_tree(workspace.path(), artifact.tree_oid()) {
            return Err(ServiceError::Artifact(
                ArtifactManager::retained_workspace_error(&workspace, error.to_string()),
            ));
        }
        artifacts
            .cleanup_unchanged_artifact_validation_workspace(
                task_id,
                &attempt_id,
                artifact_id,
                &workspace,
            )
            .map_err(ServiceError::from)?;
        artifacts
            .record_validation(task_id, artifact_id, expected_revision, result)
            .map_err(ServiceError::from)
    }

    /// Atomically accepts an asynchronous MCP validation operation.
    pub fn accept_validation(
        &self,
        caller: &str,
        request: &ValidationRunRequest,
    ) -> Result<ValidationAcceptance, ServiceError> {
        if caller.trim().is_empty()
            || request.request_id.trim().is_empty()
            || request.artifact_id.trim().is_empty()
        {
            return Err(ServiceError::InvalidRequest(
                "caller, request_id, and artifact_id must not be empty",
            ));
        }
        if request.profile_id.is_some() != request.checks.is_empty() {
            return Err(ServiceError::InvalidRequest(
                "exactly one validation profile or explicit checks is required",
            ));
        }
        for check in &request.checks {
            if check.name.trim().is_empty()
                || check.command.trim().is_empty()
                || check.timeout_ms == 0
            {
                return Err(ServiceError::InvalidRequest(
                    "validation check name, command, and positive timeout are required",
                ));
            }
        }
        if self.validation_policy.is_none() {
            return Err(ServiceError::PolicyDenied(
                "validation policy and command allowlist are not configured",
            ));
        }
        let normalized_checks = validation_specs_json(&request.checks);
        let request_json = serde_json::json!({"task_id":request.task_id.as_str(),"expected_revision":request.expected_revision,"artifact_id":request.artifact_id,"profile_id":request.profile_id,"checks":normalized_checks}).to_string();
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replay: Option<(String, String)> = tx.query_row(
            "SELECT request_json,response_json FROM service_mcp_idempotency WHERE caller=?1 AND tool_name='validation.run' AND request_id=?2",
            params![caller, request.request_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        if let Some((stored, response)) = replay {
            if stored != request_json {
                return Err(ServiceError::IdempotencyConflict);
            }
            let value: serde_json::Value =
                serde_json::from_str(&response).map_err(|_| ServiceError::InvalidStoredState)?;
            let operation_id = OperationId::new(
                value["operation_id"]
                    .as_str()
                    .ok_or(ServiceError::InvalidStoredState)?,
            );
            let accepted_at_ms = value["submitted_at_ms"]
                .as_i64()
                .ok_or(ServiceError::InvalidStoredState)?;
            let revision = value["revision"]
                .as_u64()
                .ok_or(ServiceError::InvalidStoredState)?;
            let status = ServiceOperationStatus::from_str(
                value["state"]
                    .as_str()
                    .ok_or(ServiceError::InvalidStoredState)?,
            )?;
            tx.commit()?;
            return Ok(ValidationAcceptance {
                operation_id,
                task_id: request.task_id.clone(),
                artifact_id: request.artifact_id.clone(),
                revision,
                status,
                accepted_at_ms,
            });
        }
        let actual: Option<i64> = tx
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![request.task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let actual = actual.ok_or(ServiceError::TaskNotFound)?;
        if u64::try_from(actual).ok() != Some(request.expected_revision) {
            return Err(ServiceError::StaleRevision {
                expected: request.expected_revision,
                actual: u64::try_from(actual).unwrap_or_default(),
            });
        }
        let task_state: String = tx.query_row(
            "SELECT state FROM tasks WHERE id=?1",
            params![request.task_id.as_str()],
            |row| row.get(0),
        )?;
        if matches!(task_state.as_str(), "completed" | "failed" | "cancelled") {
            return Err(ServiceError::PolicyDenied(
                "cannot validate an Artifact for a closed Task",
            ));
        }
        let artifact_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available')", params![request.task_id.as_str(), request.artifact_id], |row| row.get(0))?;
        if !artifact_exists {
            return Err(ServiceError::Artifact(ArtifactError::NotFound));
        }
        let busy: Option<String> = tx.query_row(
            "SELECT id FROM service_validation_operations WHERE task_id=?1 AND status IN ('accepted','running') UNION ALL SELECT id FROM service_operations WHERE task_id=?1 AND status IN ('accepted','running') UNION ALL SELECT id FROM service_ci_wait_operations WHERE task_id=?1 AND status IN ('accepted','running') UNION ALL SELECT id FROM service_artifact_publication_operations WHERE task_id=?1 AND status IN ('accepted','running','recovery_required') LIMIT 1",
            params![request.task_id.as_str()], |row| row.get(0),
        ).optional()?;
        if let Some(id) = busy {
            return Err(ServiceError::Busy(OperationId::new(id)));
        }
        let operation_id = OperationId::new(format!(
            "validation-op-{}-{}",
            now_ms(),
            NEXT_VALIDATION_OPERATION_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let accepted_at_ms = now_ms();
        let revision = request
            .expected_revision
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or(ServiceError::InvalidStoredState)?;
        let checks_json = validation_specs_json(&request.checks).to_string();
        let empty_profile = request.profile_id.as_deref();
        tx.execute(
            "INSERT INTO service_validation_operations(id,task_id,request_id,expected_revision,accepted_revision,revision,artifact_id,profile_id,checks_json,status,accepted_at) VALUES(?1,?2,?3,?4,?5,?5,?6,?7,?8,'accepted',?9)",
            params![operation_id.as_str(), request.task_id.as_str(), request.request_id, request.expected_revision as i64, revision as i64, request.artifact_id, empty_profile, checks_json, accepted_at_ms],
        )?;
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![request.task_id.as_str(), revision as i64],
        )?;
        let response = serde_json::json!({"operation_id":operation_id.as_str(),"revision":revision,"state":"accepted","submitted_at_ms":accepted_at_ms}).to_string();
        tx.execute("INSERT INTO service_mcp_idempotency(caller,tool_name,request_id,request_json,response_json) VALUES(?1,'validation.run',?2,?3,?4)", params![caller, request.request_id, request_json, response])?;
        tx.commit()?;
        Ok(ValidationAcceptance {
            operation_id,
            task_id: request.task_id.clone(),
            artifact_id: request.artifact_id.clone(),
            revision,
            status: ServiceOperationStatus::Accepted,
            accepted_at_ms,
        })
    }

    /// Runs one accepted validation request. Restarted running work is never replayed.
    pub fn run_validation_operation(
        &self,
        operation_id: &OperationId,
    ) -> Result<ValidationOperationSnapshot, ServiceError> {
        let request: Option<(String, i64, String, Option<String>, String, String)> = {
            let mut connection = self.ledger.lock_connection()?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let request = tx.query_row(
                "SELECT task_id,accepted_revision,artifact_id,profile_id,checks_json,status FROM service_validation_operations WHERE id=?1",
                params![operation_id.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            ).optional()?;
            let Some((task, revision, artifact, profile, checks, status)) = request else {
                return Err(ServiceError::OperationNotFound);
            };
            if status != "accepted" {
                tx.commit()?;
                return self.get_validation_operation(operation_id);
            }
            tx.execute("UPDATE service_validation_operations SET status='running',started_at=?2 WHERE id=?1 AND status='accepted'", params![operation_id.as_str(), now_ms()])?;
            tx.commit()?;
            Some((task, revision, artifact, profile, checks, status))
        };
        let (task, revision, artifact, profile, checks_json, _) =
            request.ok_or(ServiceError::OperationNotFound)?;
        let checks = validation_specs_from_json(
            serde_json::from_str(&checks_json).map_err(|_| ServiceError::InvalidStoredState)?,
        )?;
        let policy = self.validation_policy.ok_or(ServiceError::PolicyDenied(
            "validation policy and command allowlist are not configured",
        ))?;
        let adapter = PolicyValidator {
            policy,
            repository_root: self.workspaces.repository_root(),
            profile_id: profile,
            checks,
        };
        let validation_result = self.validate_artifact(
            &TaskId::new(task.clone()),
            &artifact,
            u64::try_from(revision).map_err(|_| ServiceError::InvalidStoredState)?,
            &adapter,
        );
        match validation_result {
            Ok(record) => {
                let result = serde_json::json!({"validation_id":record.id(),"artifact_id":record.artifact_id(),"state":if record.passed() {"passed"} else {"failed"},"checks":validation_checks_json(self.ledger, record.id())?});
                let result_json = result.to_string();
                let mut connection = self.ledger.lock_connection()?;
                let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let current_revision: i64 = tx.query_row(
                    "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                    params![task],
                    |row| row.get(0),
                )?;
                tx.execute("UPDATE service_validation_operations SET status='completed',validation_id=?2,result_json=?3,revision=?4,finished_at=?5 WHERE id=?1 AND status='running'", params![operation_id.as_str(), record.id(), result_json, current_revision, now_ms()])?;
                tx.commit()?;
            }
            Err(error) => {
                let code = match &error {
                    ServiceError::PolicyDenied(_) => "policy_denied",
                    ServiceError::Artifact(ArtifactError::WorkspaceRetained { .. }) => {
                        "recovery_required"
                    }
                    ServiceError::ValidationFailed => "internal_error",
                    _ => "internal_error",
                };
                let status = if code == "recovery_required" {
                    "recovery_required"
                } else {
                    "failed"
                };
                self.ledger.lock_connection()?.execute("UPDATE service_validation_operations SET status=?2,error_code=?3,finished_at=?4 WHERE id=?1 AND status='running'", params![operation_id.as_str(), status, code, now_ms()])?;
            }
        }
        self.get_validation_operation(operation_id)
    }

    #[allow(clippy::type_complexity)] // Mirrors one persisted validation snapshot row.
    pub fn get_validation_operation(
        &self,
        operation_id: &OperationId,
    ) -> Result<ValidationOperationSnapshot, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let row: Option<(String,String,String,i64,i64,Option<i64>,Option<i64>,Option<String>,Option<String>)> = connection.query_row(
            "SELECT task_id,artifact_id,status,revision,accepted_at,started_at,finished_at,result_json,error_code FROM service_validation_operations WHERE id=?1",
            params![operation_id.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
        ).optional()?;
        let Some((task, artifact, status, revision, accepted, started, finished, result, error)) =
            row
        else {
            return Err(ServiceError::OperationNotFound);
        };
        Ok(ValidationOperationSnapshot {
            operation_id: operation_id.clone(),
            task_id: TaskId::new(task),
            artifact_id: artifact,
            status: ServiceOperationStatus::from_str(&status)?,
            revision: u64::try_from(revision).map_err(|_| ServiceError::InvalidStoredState)?,
            accepted_at_ms: accepted,
            started_at_ms: started,
            finished_at_ms: finished,
            result: result
                .map(|value| {
                    serde_json::from_str(&value).map_err(|_| ServiceError::InvalidStoredState)
                })
                .transpose()?,
            error_code: error,
        })
    }

    /// Records the supervisor Codex's decision without deriving it from mechanical evidence.
    pub fn record_artifact_decision(
        &self,
        task_id: &TaskId,
        artifact_id: &str,
        expected_revision: u64,
        decision: CodexDecisionKind,
        reason: &str,
        evidence: &[(String, String)],
    ) -> Result<ArtifactCodexDecisionRecord, ServiceError> {
        ArtifactManager::new(self.workspaces, self.ledger)
            .record_decision(
                task_id,
                artifact_id,
                expected_revision,
                decision,
                reason,
                evidence,
            )
            .map_err(ServiceError::from)
    }

    /// Records a decision with durable MCP caller/request idempotency in the same SQLite transaction.
    #[allow(clippy::too_many_arguments)] // Parameters are the atomic decision record fields.
    pub fn record_artifact_decision_idempotent(
        &self,
        caller: &str,
        request_id: &str,
        task_id: &TaskId,
        artifact_id: &str,
        expected_revision: u64,
        decision: CodexDecisionKind,
        reason: &str,
        evidence: &[(String, String)],
    ) -> Result<ArtifactCodexDecisionRecord, ServiceError> {
        if request_id.trim().is_empty() {
            return Err(ServiceError::InvalidRequest("request_id must not be empty"));
        }
        ArtifactManager::new(self.workspaces, self.ledger)
            .record_decision_scoped(
                Some((caller, "decision.record", request_id)),
                task_id,
                artifact_id,
                expected_revision,
                decision,
                reason,
                evidence,
            )
            .map_err(ServiceError::from)
    }

    /// Checks exact Artifact, passing Validation and accepted Codex decision identity.
    /// This returns evidence only; it does not commit, push, scan, or create a Pull Request.
    pub fn require_artifact_publication_evidence(
        &self,
        task_id: &TaskId,
        artifact_id: &str,
        validation_id: &str,
        decision_id: &str,
        expected_revision: u64,
    ) -> Result<ArtifactPublicationPermit, ServiceError> {
        ArtifactManager::new(self.workspaces, self.ledger)
            .publication_permit(
                task_id,
                artifact_id,
                validation_id,
                decision_id,
                expected_revision,
            )
            .map_err(ServiceError::from)
    }

    /// Scans and durably accepts publication of exactly this Artifact.
    /// External effects are started separately by `run_artifact_publication`.
    /// Raw title/body and Artifact text are never persisted; only a request digest is stored.
    pub fn publish_artifact(
        &self,
        request: &ArtifactPublicationRequest,
    ) -> Result<ArtifactPublicationAcceptance, ServiceError> {
        if request.request_id.trim().is_empty() {
            return Err(ServiceError::InvalidRequest("request_id must not be empty"));
        }
        let digest = publication_request_digest(request);
        if let Some(acceptance) = self.find_publication_acceptance(&request.request_id, &digest)? {
            return Ok(acceptance);
        }
        if request.payload.base_branch() == request.payload.head_branch() {
            return Err(ServiceError::InvalidRequest(
                "publication base and head branches must differ",
            ));
        }
        if !valid_git_branch(request.payload.base_branch())
            || !valid_git_branch(request.payload.head_branch())
        {
            return Err(ServiceError::InvalidRequest(
                "publication branch name is invalid",
            ));
        }
        let gateway = self.publication_gateway.ok_or(ServiceError::PolicyDenied(
            "Artifact publication gateway is not configured",
        ))?;
        if !gateway.supports_sensitive_stdin_payload() {
            return Err(ServiceError::PolicyDenied(
                "publication gateway cannot safely send title/body on this platform",
            ));
        }
        let scanner = self.secret_scanner.ok_or(ServiceError::PolicyDenied(
            "SecretScanner is not configured",
        ))?;

        let artifacts = ArtifactManager::new(self.workspaces, self.ledger);
        let permit = artifacts
            .publication_permit(
                &request.task_id,
                &request.artifact_id,
                &request.validation_id,
                &request.decision_id,
                request.expected_revision,
            )
            .map_err(ServiceError::from)?;
        let artifact = artifacts
            .verify_input(&request.task_id, &request.artifact_id)
            .map_err(ServiceError::from)?;
        if artifact.tree_oid() != permit.tree_oid()
            || artifact.base_commit() != permit.base_commit()
        {
            return Err(ServiceError::PolicyDenied(
                "Artifact changed after evidence was checked",
            ));
        }

        let artifact_scan = scanner
            .scan_artifact_tree(artifact.repository_root(), artifact.tree_oid())
            .map_err(|_| ServiceError::PolicyDenied("secret scan could not complete safely"))?;
        let payload_scan = scanner
            .scan_publication_payload(&request.payload)
            .map_err(|_| ServiceError::PolicyDenied("secret scan could not complete safely"))?;
        if artifact_scan != SecretScanResult::Clean || payload_scan != SecretScanResult::Clean {
            return Err(ServiceError::PolicyDenied(
                "secret scan found sensitive content",
            ));
        }

        let (acceptance, _) = self.accept_artifact_publication(request, &permit, &digest)?;
        Ok(acceptance)
    }

    /// Executes an accepted publication with the exact request payload held by the caller.
    /// It rechecks the digest, evidence, Artifact tree, and secret scans before any external effect.
    pub fn run_artifact_publication(
        &self,
        acceptance: &ArtifactPublicationAcceptance,
        request: &ArtifactPublicationRequest,
    ) -> Result<ArtifactPublicationSnapshot, ServiceError> {
        if acceptance.request_id != request.request_id || acceptance.task_id != request.task_id {
            return Err(ServiceError::IdempotencyConflict);
        }
        let snapshot = self.get_artifact_publication_operation(&acceptance.operation_id)?;
        if snapshot.request_id != request.request_id
            || snapshot.task_id != request.task_id
            || snapshot.publication_id != acceptance.publication_id
        {
            return Err(ServiceError::IdempotencyConflict);
        }
        let digest = publication_request_digest(request);
        let (stored_digest, current_revision, stored_status): (String, i64, String) = {
            let connection = self.ledger.lock_connection()?;
            connection.query_row(
                "SELECT publication.request_digest,task_revision.revision,publication.status
                 FROM service_artifact_publication_operations publication
                 JOIN service_task_revisions task_revision ON task_revision.task_id=publication.task_id
                 WHERE publication.id=?1",
                params![acceptance.operation_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?
        };
        if stored_digest != digest {
            return Err(ServiceError::IdempotencyConflict);
        }
        if snapshot.state != ServiceOperationStatus::Accepted || stored_status != "accepted" {
            return self.get_artifact_publication_operation(&acceptance.operation_id);
        }
        let scanner = self.secret_scanner.ok_or(ServiceError::PolicyDenied(
            "SecretScanner is not configured",
        ))?;
        let gateway = self.publication_gateway.ok_or(ServiceError::PolicyDenied(
            "Artifact publication gateway is not configured",
        ))?;
        if !gateway.supports_sensitive_stdin_payload() {
            return Err(ServiceError::PolicyDenied(
                "publication gateway cannot safely send title/body on this platform",
            ));
        }
        let current_revision =
            u64::try_from(current_revision).map_err(|_| ServiceError::InvalidStoredState)?;
        if !self
            .set_publication_running(&acceptance.operation_id, ArtifactPublicationPhase::Accepted)?
        {
            return self.get_artifact_publication_operation(&acceptance.operation_id);
        }
        if current_revision != acceptance.revision {
            self.finish_publication(
                &acceptance.operation_id,
                ServiceOperationStatus::Failed,
                "publication_stale_revision",
                None,
                None,
            )?;
            return Err(ServiceError::StaleRevision {
                expected: acceptance.revision,
                actual: current_revision,
            });
        }
        let permit = match ArtifactManager::new(self.workspaces, self.ledger).publication_permit(
            &request.task_id,
            &request.artifact_id,
            &request.validation_id,
            &request.decision_id,
            current_revision,
        ) {
            Ok(permit) => permit,
            Err(error) => {
                self.finish_publication(
                    &acceptance.operation_id,
                    ServiceOperationStatus::Failed,
                    "publication_evidence_mismatch",
                    None,
                    None,
                )?;
                return Err(ServiceError::from(error));
            }
        };
        if permit.tree_oid() != snapshot.tree_oid {
            self.finish_publication(
                &acceptance.operation_id,
                ServiceOperationStatus::Failed,
                "publication_artifact_mismatch",
                None,
                None,
            )?;
            return Err(ServiceError::PolicyDenied(
                "Artifact changed after publication was accepted",
            ));
        }
        let artifact = match ArtifactManager::new(self.workspaces, self.ledger)
            .verify_input(&request.task_id, &request.artifact_id)
        {
            Ok(artifact) => artifact,
            Err(error) => {
                self.finish_publication(
                    &acceptance.operation_id,
                    ServiceOperationStatus::Failed,
                    "publication_artifact_unavailable",
                    None,
                    None,
                )?;
                return Err(ServiceError::from(error));
            }
        };
        let artifact_scan =
            match scanner.scan_artifact_tree(artifact.repository_root(), artifact.tree_oid()) {
                Ok(result) => result,
                Err(_) => {
                    self.finish_publication(
                        &acceptance.operation_id,
                        ServiceOperationStatus::Failed,
                        "publication_scan_failed",
                        None,
                        None,
                    )?;
                    return Err(ServiceError::PolicyDenied(
                        "secret scan could not complete safely",
                    ));
                }
            };
        let payload_scan = match scanner.scan_publication_payload(&request.payload) {
            Ok(result) => result,
            Err(_) => {
                self.finish_publication(
                    &acceptance.operation_id,
                    ServiceOperationStatus::Failed,
                    "publication_scan_failed",
                    None,
                    None,
                )?;
                return Err(ServiceError::PolicyDenied(
                    "secret scan could not complete safely",
                ));
            }
        };
        if artifact_scan != SecretScanResult::Clean || payload_scan != SecretScanResult::Clean {
            self.finish_publication(
                &acceptance.operation_id,
                ServiceOperationStatus::Failed,
                "publication_secret_detected",
                None,
                None,
            )?;
            return Err(ServiceError::PolicyDenied(
                "secret scan found sensitive content",
            ));
        }
        self.save_publication_phase(
            &acceptance.operation_id,
            ArtifactPublicationPhase::Scanned,
            None,
        )?;
        self.execute_artifact_publication(
            &acceptance.operation_id,
            &artifact,
            &permit,
            &request.payload,
            gateway,
        )?;
        self.get_artifact_publication_operation(&acceptance.operation_id)
    }

    /// Reads the durable result for a publication operation. This is separate from
    /// `get_operation`, whose current contract is provider-Attempt-specific.
    pub fn get_artifact_publication_operation(
        &self,
        operation_id: &OperationId,
    ) -> Result<ArtifactPublicationSnapshot, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let row = connection.query_row(
            "SELECT request_id,task_id,artifact_id,tree_oid,validation_id,decision_id,commit_sha,
                    pull_request_number,pull_request_url,is_draft,status,phase,revision,error_code,
                    accepted_at,finished_at,head_branch,base_branch,identity.publication_id
             FROM service_artifact_publication_operations publication
             JOIN service_publication_ids identity ON identity.operation_id=publication.id
             WHERE publication.id=?1",
            params![operation_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?, row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?, row.get::<_, Option<i64>>(9)?,
                    row.get::<_, String>(10)?, row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?, row.get::<_, Option<String>>(13)?,
                    row.get::<_, i64>(14)?, row.get::<_, Option<i64>>(15)?,
                    row.get::<_, String>(16)?, row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                ))
            },
        ).optional()?.ok_or(ServiceError::OperationNotFound)?;
        let pull_request = match (row.7, row.8, row.9) {
            (Some(number), Some(url), Some(draft)) => Some(DraftPullRequest::new(
                u64::try_from(number).map_err(|_| ServiceError::InvalidStoredState)?,
                url,
                draft != 0,
                row.6.clone().ok_or(ServiceError::InvalidStoredState)?,
                row.16.clone(),
                row.17.clone(),
            )),
            (None, None, None) => None,
            _ => return Err(ServiceError::InvalidStoredState),
        };
        Ok(ArtifactPublicationSnapshot {
            operation_id: operation_id.clone(),
            publication_id: PublicationId::new(row.18),
            request_id: row.0,
            task_id: TaskId::new(row.1),
            artifact_id: row.2,
            tree_oid: row.3,
            validation_id: row.4,
            decision_id: row.5,
            commit_sha: row.6,
            pull_request,
            state: ServiceOperationStatus::from_str(&row.10)?,
            phase: ArtifactPublicationPhase::from_str(&row.11)?,
            revision: u64::try_from(row.12).map_err(|_| ServiceError::InvalidStoredState)?,
            error_code: row.13,
            accepted_at_ms: row.14,
            finished_at_ms: row.15,
        })
    }

    /// Reads an Artifact Publication through its independent Publication ID.
    pub fn get_artifact_publication_by_id(
        &self,
        publication_id: &PublicationId,
    ) -> Result<ArtifactPublicationSnapshot, ServiceError> {
        let operation_id: Option<String> = {
            let connection = self.ledger.lock_connection()?;
            connection
                .query_row(
                    "SELECT operation_id FROM service_publication_ids WHERE publication_id=?1",
                    params![publication_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?
        };
        let operation_id =
            operation_id.ok_or(ServiceError::InvalidRequest("publication_id was not found"))?;
        self.get_artifact_publication_operation(&OperationId::new(operation_id))
    }

    /// Observes CI for a direct PR / commit target or a completed internal
    /// Publication. This Rust API does not include an MCP response envelope or
    /// transport integration.
    pub fn get_ci(&self, target: &CiServiceTarget) -> Result<CiObservation, ServiceError> {
        let (task_id, query, host) = self.resolve_ci_target(target, None)?;
        let runtime = self.ci_runtime()?;
        match host {
            Some(host) => runtime.observe_on_host(task_id.as_ref(), &query, &host),
            None => runtime.observe(task_id.as_ref(), &query),
        }
        .map_err(ServiceError::from)
    }

    /// Waits synchronously for a direct target or a completed internal
    /// Publication operation after checking the Task revision. Direct targets
    /// are attributed to the caller's Task; only Publication targets verify
    /// ownership against the saved Publication record. The full wire contract
    /// returns an asynchronous OperationAcceptance and is not exposed here.
    pub fn wait_ci(
        &self,
        task_id: &TaskId,
        expected_revision: u64,
        target: &CiServiceTarget,
        deadline: Instant,
    ) -> Result<CiObservation, ServiceError> {
        let current_revision = self.current_task_revision(task_id)?;
        if current_revision != expected_revision {
            return Err(ServiceError::StaleRevision {
                expected: expected_revision,
                actual: current_revision,
            });
        }
        let (attributed_task_id, query, host) = self.resolve_ci_target(target, Some(task_id))?;
        let attributed_task_id = attributed_task_id.ok_or(ServiceError::InvalidStoredState)?;
        let runtime = self.ci_runtime()?;
        match host {
            Some(host) => runtime.wait_on_host(&attributed_task_id, &query, deadline, &host),
            None => runtime.wait(&attributed_task_id, &query, deadline),
        }
        .map_err(ServiceError::from)
    }

    /// Persist an asynchronous CI wait request and return its acceptance.
    /// The caller schedules `run_ci_wait_operation` separately.
    pub fn accept_ci_wait(
        &self,
        caller: &str,
        request: &CiWaitRequest,
    ) -> Result<CiWaitAcceptance, ServiceError> {
        if caller.trim().is_empty() {
            return Err(ServiceError::InvalidRequest(
                "caller identity must not be empty",
            ));
        }
        if request.request_id.trim().is_empty() {
            return Err(ServiceError::InvalidRequest("request_id must not be empty"));
        }
        let deadline = request
            .deadline
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ServiceError::InvalidRequest("CI deadline must be after Unix epoch"))?;
        let deadline_seconds: i64 = deadline
            .as_secs()
            .try_into()
            .map_err(|_| ServiceError::InvalidRequest("CI deadline is out of range"))?;
        let deadline_nanos = i64::from(deadline.subsec_nanos());
        let expected_revision: i64 = request
            .expected_revision
            .try_into()
            .map_err(|_| ServiceError::InvalidRequest("revision is out of range"))?;
        let target_json = ci_target_json(&request.target);
        // Resolve an idempotent replay before checking the live revision; the
        // original acceptance remains authoritative even after Task changes.
        let previous: Option<(String, String, String, i64, i64, i64)> = {
            let connection = self.ledger.lock_connection()?;
            connection.query_row(
                "SELECT id,task_id,target_json,deadline_seconds,deadline_nanos,expected_revision FROM service_ci_wait_operations WHERE caller=?1 AND tool_name='ci.wait' AND request_id=?2",
                params![caller,request.request_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            ).optional()?
        };
        if let Some((id, task, stored_target, stored_seconds, stored_nanos, revision)) = previous {
            if task != request.task_id.as_str()
                || stored_target != target_json
                || stored_seconds != deadline_seconds
                || stored_nanos != deadline_nanos
                || revision != expected_revision
            {
                return Err(ServiceError::IdempotencyConflict);
            }
            let snapshot = self.get_ci_wait_operation(&OperationId::new(id))?;
            return Ok(CiWaitAcceptance {
                operation_id: snapshot.operation_id,
                revision: snapshot.revision,
                status: snapshot.status,
            });
        }
        self.ci_runtime()?;
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String,String,String,i64,i64,i64)> = tx.query_row(
            "SELECT id,task_id,target_json,deadline_seconds,deadline_nanos,expected_revision FROM service_ci_wait_operations WHERE caller=?1 AND tool_name='ci.wait' AND request_id=?2",
            params![caller,request.request_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        ).optional()?;
        if let Some((id, task, stored_target, stored_seconds, stored_nanos, revision)) = existing {
            if task != request.task_id.as_str()
                || stored_target != target_json
                || stored_seconds != deadline_seconds
                || stored_nanos != deadline_nanos
                || revision != expected_revision
            {
                return Err(ServiceError::IdempotencyConflict);
            }
            tx.commit()?;
            drop(connection);
            let snapshot = self.get_ci_wait_operation(&OperationId::new(id))?;
            return Ok(CiWaitAcceptance {
                operation_id: snapshot.operation_id,
                revision: snapshot.revision,
                status: snapshot.status,
            });
        }
        let direct_query = match &request.target {
            CiServiceTarget::Publication(_) => None,
            CiServiceTarget::PullRequest { repository, number } => {
                Some(CiQueryTarget::PullRequest {
                    repository: repository.clone(),
                    number: *number,
                    expected_head_sha: None,
                })
            }
            CiServiceTarget::Commit { repository, sha } => Some(CiQueryTarget::Commit {
                repository: repository.clone(),
                sha: sha.clone(),
            }),
        };
        if let Some(query) = direct_query {
            crate::ci::validate_query_target(&query)
                .map_err(|_| ServiceError::InvalidRequest("invalid CI target"))?;
        }
        // Revision, Task state and Publication binding are read under the same
        // immediate transaction as the acceptance insert, closing stale-accept
        // races with Task mutations.
        let actual: Option<i64> = tx
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![request.task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let actual = actual.ok_or(ServiceError::TaskNotFound)?;
        let actual_u64 = u64::try_from(actual).map_err(|_| ServiceError::InvalidStoredState)?;
        if actual_u64 != request.expected_revision {
            return Err(ServiceError::StaleRevision {
                expected: request.expected_revision,
                actual: actual_u64,
            });
        }
        let task_state: String = tx
            .query_row(
                "SELECT state FROM tasks WHERE id=?1",
                params![request.task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ServiceError::TaskNotFound)?;
        if matches!(task_state.as_str(), "completed" | "cancelled") {
            return Err(ServiceError::PolicyDenied(
                "CI wait requires an active Task",
            ));
        }
        resolve_ci_target_in_connection(&tx, &request.target, Some(&request.task_id))?;
        // Check expiry only after both idempotency lookups. Replaying an
        // accepted request remains valid after its original deadline.
        if request.deadline <= SystemTime::now() {
            return Err(ServiceError::InvalidRequest(
                "CI deadline must be in the future",
            ));
        }
        let id = format!(
            "ci-wait-{}-{}",
            now_ms(),
            NEXT_CI_WAIT_OPERATION_ID.fetch_add(1, Ordering::Relaxed)
        );
        tx.execute("INSERT INTO service_ci_wait_operations(id,caller,tool_name,request_id,task_id,expected_revision,target_json,deadline_seconds,deadline_nanos,status,accepted_at) VALUES(?1,?2,'ci.wait',?3,?4,?5,?6,?7,?8,'accepted',?9)",
            params![id,caller,request.request_id,request.task_id.as_str(),expected_revision,target_json,deadline_seconds,deadline_nanos,now_ms()])?;
        tx.commit()?;
        Ok(CiWaitAcceptance {
            operation_id: OperationId::new(id),
            revision: request.expected_revision,
            status: ServiceOperationStatus::Accepted,
        })
    }

    /// Execute an accepted CI wait once. Repeated calls return stored state and
    /// never repeat an operation that has already been claimed.
    pub fn run_ci_wait_operation(
        &self,
        operation_id: &OperationId,
    ) -> Result<CiWaitOperationSnapshot, ServiceError> {
        let token = CancellationToken::new();
        self.active_cancel_tokens
            .lock()
            .map_err(|_| ServiceError::InvalidStoredState)?
            .insert(operation_id.as_str().to_owned(), token.clone());
        let _active_guard = ActiveCancellationGuard {
            registry: &self.active_cancel_tokens,
            operation_id: operation_id.as_str().to_owned(),
        };
        self.run_ci_wait_operation_inner(operation_id, &token)
    }

    fn run_ci_wait_operation_inner(
        &self,
        operation_id: &OperationId,
        cancellation: &CancellationToken,
    ) -> Result<CiWaitOperationSnapshot, ServiceError> {
        let current = self.get_ci_wait_operation(operation_id)?;
        if current.status() != ServiceOperationStatus::Accepted {
            return Ok(current);
        }
        // Check the injected observer before claiming the operation, so a
        // configuration error leaves the durable operation retryable.
        let runtime = self.ci_runtime()?;
        let claim = {
            let mut connection = self.ledger.lock_connection()?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row: (String,String,String,i64,i64,String) = tx.query_row(
                "SELECT task_id,status,target_json,deadline_seconds,deadline_nanos,request_id FROM service_ci_wait_operations WHERE id=?1",
                params![operation_id.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            ).optional()?.ok_or(ServiceError::OperationNotFound)?;
            if row.1 != "accepted" {
                if row.1 == "cancelling" {
                    tx.execute("UPDATE service_ci_wait_operations SET status='cancelled',error_code='cancelled',finished_at=?2 WHERE id=?1 AND status='cancelling'",params![operation_id.as_str(),now_ms()])?;
                }
                tx.commit()?;
                drop(connection);
                return self.get_ci_wait_operation(operation_id);
            }
            tx.execute("UPDATE service_ci_wait_operations SET status='running',started_at=?2 WHERE id=?1 AND status='accepted'",params![operation_id.as_str(),now_ms()])?;
            self.ledger
                .register_ci_wait_cancellation(operation_id.as_str(), cancellation.clone())?;
            let registration_guard = ActiveCiWaitGuard {
                ledger: self.ledger,
                operation_id: operation_id.as_str().to_owned(),
                token: cancellation.clone(),
            };
            tx.commit()?;
            (row, registration_guard)
        };
        let (claim, _registration_guard) = claim;
        let prepared = (|| {
            let task_id = TaskId::new(claim.0);
            let target = parse_ci_target(&claim.2)?;
            let deadline = UNIX_EPOCH
                .checked_add(Duration::new(
                    u64::try_from(claim.3).map_err(|_| ServiceError::InvalidStoredState)?,
                    u32::try_from(claim.4).map_err(|_| ServiceError::InvalidStoredState)?,
                ))
                .ok_or(ServiceError::InvalidStoredState)?;
            let remaining = deadline
                .duration_since(SystemTime::now())
                .unwrap_or_default();
            let resolved = self.resolve_ci_target(&target, Some(&task_id))?;
            let run_deadline = Instant::now()
                .checked_add(remaining)
                .ok_or(ServiceError::InvalidStoredState)?;
            Ok::<_, ServiceError>((task_id, resolved, run_deadline))
        })();
        let (task_id, resolved, run_deadline) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let (status, code) = match error {
                    ServiceError::Ledger(_) | ServiceError::Sqlite(_) => {
                        ("recovery_required", "recovery_required")
                    }
                    _ => ("failed", "internal_error"),
                };
                self.finish_ci_wait(operation_id, status, None, Some(code), None)?;
                return self.get_ci_wait_operation(operation_id);
            }
        };
        let result = match resolved.2 {
            Some(host) => runtime.wait_on_host_with_cancellation(
                &task_id,
                &resolved.1,
                run_deadline,
                &host,
                cancellation,
            ),
            None => {
                runtime.wait_with_cancellation(&task_id, &resolved.1, run_deadline, cancellation)
            }
        };
        match result {
            Ok(observation) => self.finish_ci_wait(
                operation_id,
                "completed",
                Some(observation.id()),
                None,
                None,
            )?,
            Err(CiError::Timeout {
                last_observation_id,
            }) => self.finish_ci_wait(
                operation_id,
                "failed",
                None,
                Some("timeout"),
                last_observation_id.as_deref(),
            )?,
            Err(CiError::Cancelled {
                last_observation_id,
            }) => self.finish_ci_wait(
                operation_id,
                "cancelled",
                None,
                Some("cancelled"),
                last_observation_id.as_deref(),
            )?,
            Err(CiError::Ledger(_)) => self.finish_ci_wait(
                operation_id,
                "recovery_required",
                None,
                Some("recovery_required"),
                None,
            )?,
            Err(CiError::Unavailable {
                last_observation_id,
                ..
            }) => self.finish_ci_wait(
                operation_id,
                "failed",
                None,
                Some("internal_error"),
                last_observation_id.as_deref(),
            )?,
            Err(CiError::HeadChanged {
                last_observation_id,
                ..
            }) => self.finish_ci_wait(
                operation_id,
                "failed",
                None,
                Some("internal_error"),
                Some(&last_observation_id),
            )?,
            Err(_) => {
                self.finish_ci_wait(operation_id, "failed", None, Some("internal_error"), None)?
            }
        }
        self.get_ci_wait_operation(operation_id)
    }

    #[allow(clippy::type_complexity)] // Mirrors one persisted CI wait snapshot row.
    pub fn get_ci_wait_operation(
        &self,
        operation_id: &OperationId,
    ) -> Result<CiWaitOperationSnapshot, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let row: (String,String,Option<String>,Option<String>,Option<String>,i64,i64,Option<i64>,Option<i64>) = connection.query_row(
            "SELECT task_id,status,observation_id,error_code,details_ref,expected_revision,accepted_at,started_at,finished_at FROM service_ci_wait_operations WHERE id=?1",
            params![operation_id.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
        ).optional()?.ok_or(ServiceError::OperationNotFound)?;
        drop(connection);
        let status = match row.1.as_str() {
            "accepted" => ServiceOperationStatus::Accepted,
            "running" => ServiceOperationStatus::Running,
            "cancelling" => ServiceOperationStatus::Cancelling,
            "completed" => ServiceOperationStatus::Completed,
            "failed" => ServiceOperationStatus::Failed,
            "cancelled" => ServiceOperationStatus::Cancelled,
            "recovery_required" => ServiceOperationStatus::RecoveryRequired,
            _ => return Err(ServiceError::InvalidStoredState),
        };
        let observation = if status == ServiceOperationStatus::Completed {
            row.2
                .as_deref()
                .map(|id| self.ledger.get_ci_observation(id))
                .transpose()?
                .flatten()
        } else {
            None
        };
        Ok(CiWaitOperationSnapshot {
            operation_id: operation_id.clone(),
            task_id: TaskId::new(row.0),
            status,
            observation,
            error_code: row.3,
            details_ref: row.4,
            revision: u64::try_from(row.5).map_err(|_| ServiceError::InvalidStoredState)?,
            accepted_at_ms: row.6,
            started_at_ms: row.7,
            finished_at_ms: row.8,
        })
    }

    /// Requests cancellation of an accepted/running CI wait target. The
    /// general `operation.cancel` acceptance adapter calls this target hook;
    /// this method itself is not that separate acceptance operation.
    pub fn request_ci_wait_cancellation(
        &self,
        operation_id: &OperationId,
    ) -> Result<CiWaitCancelTargetState, ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: String = tx
            .query_row(
                "SELECT status FROM service_ci_wait_operations WHERE id=?1",
                params![operation_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ServiceError::OperationNotFound)?;
        let target = match status.as_str() {
            "accepted" => {
                tx.execute("UPDATE service_ci_wait_operations SET status='cancelled',error_code='cancelled',finished_at=?2 WHERE id=?1 AND status='accepted'",params![operation_id.as_str(),now_ms()])?;
                CiWaitCancelTargetState::Cancelled
            }
            "running" | "cancelling" => {
                let token = self
                    .ledger
                    .get_ci_wait_cancellation(operation_id.as_str())?;
                if token.is_some() {
                    tx.execute("UPDATE service_ci_wait_operations SET status='cancelling' WHERE id=?1 AND status IN ('running','cancelling')",params![operation_id.as_str()])?;
                    CiWaitCancelTargetState::Running
                } else {
                    tx.execute("UPDATE service_ci_wait_operations SET status='recovery_required',error_code='cancel_worker_unavailable',finished_at=?2 WHERE id=?1 AND status IN ('running','cancelling')",params![operation_id.as_str(),now_ms()])?;
                    CiWaitCancelTargetState::RecoveryRequired
                }
            }
            "recovery_required" => CiWaitCancelTargetState::RecoveryRequired,
            _ => {
                return Err(ServiceError::PolicyDenied(
                    "CI wait operation is already terminal",
                ));
            }
        };
        tx.commit()?;
        drop(connection);
        if target == CiWaitCancelTargetState::Running {
            if let Some(token) = self
                .ledger
                .get_ci_wait_cancellation(operation_id.as_str())?
            {
                token.cancel();
            }
        }
        Ok(target)
    }

    /// Target-side delegate used by `task.cancel` acceptance to request stop
    /// for each active CI wait belonging to the Task. It is not the
    /// contract-level `task.cancel` acceptance and does not mark the Task
    /// cancelled; the outer operation coordinates every active kind.
    pub fn request_task_ci_wait_cancellations(
        &self,
        task_id: &TaskId,
    ) -> Result<Vec<(OperationId, CiWaitCancelTargetState)>, ServiceError> {
        let ids = {
            let connection = self.ledger.lock_connection()?;
            let mut statement = connection.prepare("SELECT id FROM service_ci_wait_operations WHERE task_id=?1 AND status IN ('accepted','running','cancelling') ORDER BY accepted_at,id")?;
            statement
                .query_map(params![task_id.as_str()], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        ids.into_iter()
            .map(|id| {
                let operation_id = OperationId::new(id);
                let state = self.request_ci_wait_cancellation(&operation_id)?;
                Ok((operation_id, state))
            })
            .collect()
    }

    /// Additive union getter for operation kinds currently implemented by this
    /// Service. Existing attempt-specific `get` remains source compatible.
    pub fn get_operation_result(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationGetResult, ServiceError> {
        match self.get_operation(operation_id) {
            Ok(snapshot) => Ok(OperationGetResult::Attempt(snapshot)),
            Err(ServiceError::OperationNotFound) => {
                match self.get_ci_wait_operation(operation_id) {
                    Ok(snapshot) => Ok(OperationGetResult::CiWait(snapshot)),
                    Err(ServiceError::OperationNotFound) => {
                        match self.get_artifact_publication_operation(operation_id) {
                            Ok(snapshot) => Ok(OperationGetResult::ArtifactPublication(snapshot)),
                            Err(ServiceError::OperationNotFound) => {
                                match self.get_validation_operation(operation_id) {
                                    Ok(snapshot) => Ok(OperationGetResult::Validation(snapshot)),
                                    Err(ServiceError::OperationNotFound) => self
                                        .get_cancellation_operation(operation_id)
                                        .map(OperationGetResult::Cancellation),
                                    Err(error) => Err(error),
                                }
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Verifies the referenced operation before refusing log reads when no
    /// configured redactor and retained safe log store are available. Raw
    /// Provider output is never used as a fallback.
    /// Reads a Task snapshot and persists section cursors bound to its revision.
    pub fn get_task_context(
        &self,
        task_id: &TaskId,
        sections: &[String],
        page_size: usize,
        cursors: &std::collections::BTreeMap<String, String>,
    ) -> Result<serde_json::Value, ServiceError> {
        if sections.is_empty()
            || page_size == 0
            || page_size > 100
            || sections
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != sections.len()
            || cursors.keys().any(|section| !sections.contains(section))
        {
            return Err(ServiceError::InvalidRequest("invalid context page request"));
        }
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let (state, revision, request_json): (String, i64, String) = tx.query_row(
            "SELECT tasks.state,service_task_revisions.revision,task_request_snapshots.request_json FROM tasks JOIN service_task_revisions ON service_task_revisions.task_id=tasks.id JOIN task_request_snapshots ON task_request_snapshots.task_id=tasks.id WHERE tasks.id=?1",
            params![task_id.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?.ok_or(ServiceError::TaskNotFound)?;
        let request: serde_json::Value =
            serde_json::from_str(&request_json).map_err(|_| ServiceError::InvalidStoredState)?;
        let revision_u64 = u64::try_from(revision).map_err(|_| ServiceError::InvalidStoredState)?;
        let mut page_map = serde_json::Map::new();
        for section in sections {
            let mut all = context_items(&tx, task_id, section)?;
            all.sort_by(|a, b| {
                b["_at"]
                    .as_i64()
                    .cmp(&a["_at"].as_i64())
                    .then_with(|| b["id"].as_str().cmp(&a["id"].as_str()))
            });
            let offset = if let Some(cursor) = cursors.get(section) {
                tx.query_row("SELECT offset FROM service_context_cursors WHERE cursor=?1 AND task_id=?2 AND section=?3 AND page_size=?4 AND revision=?5",params![cursor,task_id.as_str(),section,page_size as i64,revision],|row|row.get::<_,i64>(0)).optional()?.ok_or(ServiceError::InvalidCursor)? as usize
            } else {
                0
            };
            if offset > all.len() {
                return Err(ServiceError::InvalidCursor);
            }
            let end = offset.saturating_add(page_size).min(all.len());
            let mut visible = Vec::new();
            for mut item in all[offset..end].iter().cloned() {
                if let Some(obj) = item.as_object_mut() {
                    obj.remove("_at");
                }
                visible.push(item);
            }
            let next_cursor = if end < all.len() {
                let cursor = format!(
                    "ctx-{}-{}",
                    now_ms(),
                    NEXT_CONTEXT_CURSOR_ID.fetch_add(1, Ordering::Relaxed)
                );
                tx.execute("INSERT INTO service_context_cursors(cursor,task_id,section,page_size,revision,offset) VALUES(?1,?2,?3,?4,?5,?6)",params![cursor,task_id.as_str(),section,page_size as i64,revision,end as i64])?;
                Some(cursor)
            } else {
                None
            };
            page_map.insert(
                section.clone(),
                serde_json::json!({"items":visible,"next_cursor":next_cursor}),
            );
        }
        tx.commit()?;
        Ok(
            serde_json::json!({"schema_version":"v2","task":{"task_id":task_id.as_str(),"revision":revision_u64,"state":state,"request":request},"sections":page_map,"observed_at":context_timestamp(now_ms())}),
        )
    }

    pub fn list_operation_logs(&self, operation_id: &OperationId) -> Result<(), ServiceError> {
        self.get_operation_result(operation_id)?;
        Err(ServiceError::Forbidden(
            "redacted operation log storage is not configured",
        ))
    }

    #[allow(clippy::type_complexity)] // Mirrors one persisted cancellation snapshot row.
    pub fn get_cancellation_operation(
        &self,
        id: &OperationId,
    ) -> Result<CancellationOperationSnapshot, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let row:(String,String,String,String,Option<String>,Option<String>,i64,i64,Option<i64>,Option<i64>)=connection.query_row("SELECT task_id,kind,status,target_ids_json,target_states_json,error_code,revision,accepted_at,started_at,finished_at FROM service_cancellation_operations WHERE id=?1",params![id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))).optional()?.ok_or(ServiceError::OperationNotFound)?;
        let status = ServiceOperationStatus::from_str(&row.2)
            .map_err(|_| ServiceError::InvalidStoredState)?;
        let targets: Vec<Value> =
            serde_json::from_str(&row.3).map_err(|_| ServiceError::InvalidStoredState)?;
        let result = row
            .4
            .map(|json| serde_json::from_str(&json).map_err(|_| ServiceError::InvalidStoredState))
            .transpose()?;
        let _ = targets;
        Ok(CancellationOperationSnapshot {
            operation_id: id.clone(),
            task_id: TaskId::new(row.0),
            kind: row.1,
            status,
            revision: u64::try_from(row.6).map_err(|_| ServiceError::InvalidStoredState)?,
            accepted_at_ms: row.7,
            started_at_ms: row.8,
            finished_at_ms: row.9,
            result,
            error_code: row.5,
        })
    }

    pub fn run_cancellation_operation(
        &self,
        id: &OperationId,
    ) -> Result<CancellationOperationSnapshot, ServiceError> {
        let current = self.get_cancellation_operation(id)?;
        if current.status() != ServiceOperationStatus::Accepted {
            return Ok(current);
        }
        {
            let connection = self.ledger.lock_connection()?;
            connection.execute("UPDATE service_cancellation_operations SET status='running',started_at=?2 WHERE id=?1 AND status='accepted'",params![id.as_str(),now_ms()])?;
        }
        let connection = self.ledger.lock_connection()?;
        let (task_id, kind, target_json): (String, String, String) = connection.query_row(
            "SELECT task_id,kind,target_ids_json FROM service_cancellation_operations WHERE id=?1",
            params![id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let targets: Vec<Value> =
            serde_json::from_str(&target_json).map_err(|_| ServiceError::InvalidStoredState)?;
        drop(connection);
        if targets.iter().any(|target| {
            matches!(
                target["kind"].as_str(),
                Some("validation.run" | "publication.publish")
            )
        }) {
            let c = self.ledger.lock_connection()?;
            c.execute("UPDATE service_cancellation_operations SET status='failed',error_code='not_cancellable',finished_at=?2 WHERE id=?1 AND status='running'", params![id.as_str(), now_ms()])?;
            drop(c);
            return self.get_cancellation_operation(id);
        }
        let mut observations = Vec::new();
        for target in &targets {
            let target_kind = target["kind"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?;
            let target_id = target["id"]
                .as_str()
                .ok_or(ServiceError::InvalidStoredState)?;
            let state = self.request_target_stop(target_kind, target_id)?;
            observations.push(serde_json::json!({"kind":target_kind,"id":target_id,"state":state}));
        }
        let can_wait = targets
            .iter()
            .all(|target| matches!(target["kind"].as_str(), Some("attempt.run" | "ci.wait")));
        if can_wait {
            loop {
                let mut all_terminal = true;
                observations.clear();
                for target in &targets {
                    let target_kind = target["kind"]
                        .as_str()
                        .ok_or(ServiceError::InvalidStoredState)?;
                    let target_id = target["id"]
                        .as_str()
                        .ok_or(ServiceError::InvalidStoredState)?;
                    let state = self.target_stop_state(target_kind, target_id)?;
                    if state == "running" {
                        all_terminal = false;
                    }
                    observations
                        .push(serde_json::json!({"kind":target_kind,"id":target_id,"state":state}));
                }
                if all_terminal {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let all_cancelled = observations.iter().all(|v| v["state"] == "cancelled");
        let unrepresentable = observations.iter().any(|v| v["state"] == "finished");
        if kind == "operation.cancel" && unrepresentable {
            let c = self.ledger.lock_connection()?;
            c.execute("UPDATE service_cancellation_operations SET status='failed',error_code='invalid_state_transition',finished_at=?2 WHERE id=?1",params![id.as_str(),now_ms()])?;
            drop(c);
            return self.get_cancellation_operation(id);
        }
        let result = if kind == "operation.cancel" {
            serde_json::json!({"target_operation_id":targets.first().and_then(|v|v["id"].as_str()),"target_state":observations.first().and_then(|v|v["state"].as_str()).unwrap_or("recovery_required")})
        } else {
            serde_json::json!({"task_state":if all_cancelled{"cancelled"}else{"active"},"cancelled_operation_ids":observations.iter().filter(|v|v["state"]=="cancelled").filter_map(|v|v["id"].as_str()).collect::<Vec<_>>()})
        };
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE service_cancellation_operations SET status='completed',target_states_json=?2,finished_at=?3 WHERE id=?1 AND status='running'",params![id.as_str(),result.to_string(),now_ms()])?;
        if kind == "task.cancel" {
            let task_state = if all_cancelled { "cancelled" } else { "active" };
            let changed = tx.execute(
                "UPDATE tasks SET state=?2 WHERE id=?1 AND state<>?2",
                params![task_id, task_state],
            )?;
            if changed == 1 {
                tx.execute(
                    "UPDATE service_task_revisions SET revision=revision+1 WHERE task_id=?1",
                    params![task_id],
                )?;
                tx.execute(
                    "UPDATE service_cancellation_operations SET revision=revision+1 WHERE id=?1",
                    params![id.as_str()],
                )?;
            }
        }
        tx.commit()?;
        drop(connection);
        self.get_cancellation_operation(id)
    }

    fn request_target_stop(
        &self,
        kind: &str,
        target_id: &str,
    ) -> Result<&'static str, ServiceError> {
        let operation_id = OperationId::new(target_id);
        match kind {
            "attempt.run" => {
                let status: String = {
                    let c = self.ledger.lock_connection()?;
                    c.query_row(
                        "SELECT status FROM service_operations WHERE id=?1",
                        params![target_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or(ServiceError::OperationNotFound)?
                };
                if status == "accepted" {
                    let c = self.ledger.lock_connection()?;
                    let (task, attempt): (String, String) = c.query_row(
                        "SELECT task_id,attempt_id FROM service_operations WHERE id=?1",
                        params![target_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    c.execute("UPDATE service_operations SET status='cancelled',diagnostic_code='cancelled',finished_at=?2 WHERE id=?1 AND status='accepted'",params![target_id,now_ms()])?;
                    c.execute("UPDATE attempts SET state='cancelled',finished_at=?3,failure_reason='cancelled' WHERE task_id=?1 AND id=?2",params![task,attempt,now_ms()])?;
                    Ok("cancelled")
                } else if let Some(token) = self
                    .active_cancel_tokens
                    .lock()
                    .map_err(|_| ServiceError::InvalidStoredState)?
                    .get(target_id)
                    .cloned()
                {
                    token.cancel();
                    Ok("running")
                } else {
                    Ok("recovery_required")
                }
            }
            "ci.wait" => match self.request_ci_wait_cancellation(&operation_id)? {
                CiWaitCancelTargetState::Cancelled => Ok("cancelled"),
                CiWaitCancelTargetState::Running => Ok("running"),
                CiWaitCancelTargetState::RecoveryRequired => Ok("recovery_required"),
            },
            "validation.run" | "publication.publish" => {
                Ok(self.target_stop_state(kind, target_id)?)
            }
            _ => Err(ServiceError::InvalidStoredState),
        }
    }

    fn target_stop_state(&self, kind: &str, id: &str) -> Result<&'static str, ServiceError> {
        let c = self.ledger.lock_connection()?;
        let (table, completed, cancelled, recovery) = match kind {
            "attempt.run" => (
                "service_operations",
                "completed",
                "cancelled",
                "recovery_required",
            ),
            "ci.wait" => (
                "service_ci_wait_operations",
                "completed",
                "cancelled",
                "recovery_required",
            ),
            "validation.run" => (
                "service_validation_operations",
                "completed",
                "cancelled",
                "recovery_required",
            ),
            "publication.publish" => (
                "service_artifact_publication_operations",
                "completed",
                "cancelled",
                "recovery_required",
            ),
            _ => return Err(ServiceError::InvalidStoredState),
        };
        let sql = format!("SELECT status FROM {table} WHERE id=?1");
        let state: String = c
            .query_row(&sql, params![id], |r| r.get(0))
            .optional()?
            .ok_or(ServiceError::OperationNotFound)?;
        Ok(if state == cancelled {
            "cancelled"
        } else if state == recovery {
            "recovery_required"
        } else if state == completed || matches!(state.as_str(), "failed") {
            "finished"
        } else {
            "running"
        })
    }

    #[allow(clippy::too_many_arguments)] // Parameters are the caller-scoped cancellation contract fields.
    pub fn accept_cancellation(
        &self,
        caller: &str,
        request_id: &str,
        task_id: &TaskId,
        expected_revision: u64,
        kind: &'static str,
        target_operation_id: Option<&OperationId>,
        reason: Option<&str>,
    ) -> Result<CancellationAcceptance, ServiceError> {
        if caller.is_empty()
            || request_id.is_empty()
            || !matches!(kind, "operation.cancel" | "task.cancel")
        {
            return Err(ServiceError::InvalidRequest(
                "invalid cancellation identity or kind",
            ));
        }
        if (kind == "operation.cancel") != target_operation_id.is_some() {
            return Err(ServiceError::InvalidRequest(
                "operation.cancel requires one target",
            ));
        }
        let request_json=serde_json::json!({"task_id":task_id.as_str(),"expected_revision":expected_revision,"target_operation_id":target_operation_id.map(OperationId::as_str),"reason":reason}).to_string();
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((saved_request,saved_response))=tx.query_row("SELECT request_json,response_json FROM service_mcp_idempotency WHERE caller=?1 AND tool_name=?2 AND request_id=?3",params![caller,kind,request_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()? {
            if saved_request!=request_json {return Err(ServiceError::IdempotencyConflict);}
            let value:Value=serde_json::from_str(&saved_response).map_err(|_|ServiceError::InvalidStoredState)?;
            return Ok(CancellationAcceptance{operation_id:OperationId::new(value["operation_id"].as_str().ok_or(ServiceError::InvalidStoredState)?),task_id:TaskId::new(value["task_id"].as_str().ok_or(ServiceError::InvalidStoredState)?),kind,revision:value["revision"].as_u64().ok_or(ServiceError::InvalidStoredState)?,accepted_at_ms:value["accepted_at_ms"].as_i64().ok_or(ServiceError::InvalidStoredState)?});
        }
        let (state,actual):(String,i64)=tx.query_row("SELECT tasks.state,service_task_revisions.revision FROM tasks JOIN service_task_revisions ON service_task_revisions.task_id=tasks.id WHERE tasks.id=?1",params![task_id.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(ServiceError::TaskNotFound)?;
        if state != "pending" && state != "active" {
            return Err(ServiceError::NotCancellable);
        }
        if u64::try_from(actual).ok() != Some(expected_revision) {
            return Err(ServiceError::StaleRevision {
                expected: expected_revision,
                actual: u64::try_from(actual).map_err(|_| ServiceError::InvalidStoredState)?,
            });
        }
        let mut targets: Vec<Value> = Vec::new();
        if let Some(target) = target_operation_id {
            let found: Option<(String, String)> = tx
                .query_row(
                    "SELECT task_id,status FROM service_operations WHERE id=?1",
                    params![target.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((owner, status)) = found {
                if owner != task_id.as_str()
                    || !matches!(status.as_str(), "accepted" | "running" | "cancelling")
                {
                    return Err(ServiceError::NotCancellable);
                }
                targets.push(serde_json::json!({"kind":"attempt.run","id":target.as_str()}));
            } else {
                let found: Option<(String, String)> = tx
                    .query_row(
                        "SELECT task_id,status FROM service_ci_wait_operations WHERE id=?1",
                        params![target.as_str()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                if let Some((owner, status)) = found {
                    if owner != task_id.as_str()
                        || !matches!(status.as_str(), "accepted" | "running" | "cancelling")
                    {
                        return Err(ServiceError::NotCancellable);
                    }
                    targets.push(serde_json::json!({"kind":"ci.wait","id":target.as_str()}));
                } else {
                    let found:Option<(String,String,String)>=tx.query_row("SELECT task_id,status,'validation.run' FROM service_validation_operations WHERE id=?1",params![target.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
                    if let Some((owner, status, kind)) = found {
                        if owner != task_id.as_str()
                            || !matches!(status.as_str(), "accepted" | "running")
                        {
                            return Err(ServiceError::NotCancellable);
                        }
                        let _ = kind;
                        return Err(ServiceError::NotCancellable);
                    } else {
                        let found:Option<(String,String)>=tx.query_row("SELECT task_id,status FROM service_artifact_publication_operations WHERE id=?1",params![target.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                        if let Some((owner, status)) = found {
                            if owner != task_id.as_str()
                                || !matches!(status.as_str(), "accepted" | "running")
                            {
                                return Err(ServiceError::NotCancellable);
                            }
                            return Err(ServiceError::NotCancellable);
                        } else {
                            return Err(ServiceError::OperationNotFound);
                        }
                    }
                }
            }
        } else {
            for (kind, table) in [
                ("attempt.run", "service_operations"),
                ("ci.wait", "service_ci_wait_operations"),
                ("validation.run", "service_validation_operations"),
                (
                    "publication.publish",
                    "service_artifact_publication_operations",
                ),
            ] {
                let sql = format!(
                    "SELECT id FROM {table} WHERE task_id=?1 AND status IN ('accepted','running','cancelling') ORDER BY accepted_at,id"
                );
                let mut stmt = tx.prepare(&sql)?;
                let ids = stmt
                    .query_map(params![task_id.as_str()], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                if matches!(kind, "validation.run" | "publication.publish") && !ids.is_empty() {
                    return Err(ServiceError::NotCancellable);
                }
                targets.extend(
                    ids.into_iter()
                        .map(|id| serde_json::json!({"kind":kind,"id":id})),
                );
            }
        }
        let new_revision = actual
            .checked_add(1)
            .ok_or(ServiceError::InvalidStoredState)?;
        let now = now_ms();
        let operation_id = OperationId::new(format!(
            "cancel-{now}-{}",
            NEXT_CANCELLATION_OPERATION_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let response=serde_json::json!({"operation_id":operation_id.as_str(),"task_id":task_id.as_str(),"revision":new_revision,"accepted_at_ms":now}).to_string();
        tx.execute("INSERT INTO service_cancellation_operations(id,task_id,kind,target_operation_id,target_ids_json,status,accepted_revision,revision,accepted_at,reason) VALUES(?1,?2,?3,?4,?5,'accepted',?6,?7,?8,?9)",params![operation_id.as_str(),task_id.as_str(),kind,target_operation_id.map(OperationId::as_str),serde_json::to_string(&targets).map_err(|_|ServiceError::InvalidStoredState)?,actual,new_revision,now,reason])?;
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![task_id.as_str(), new_revision],
        )?;
        tx.execute("INSERT INTO service_mcp_idempotency(caller,tool_name,request_id,request_json,response_json) VALUES(?1,?2,?3,?4,?5)",params![caller,kind,request_id,request_json,response])?;
        tx.commit()?;
        Ok(CancellationAcceptance {
            operation_id,
            task_id: task_id.clone(),
            kind,
            revision: u64::try_from(new_revision).map_err(|_| ServiceError::InvalidStoredState)?,
            accepted_at_ms: now,
        })
    }

    fn finish_ci_wait(
        &self,
        id: &OperationId,
        status: &str,
        observation_id: Option<&str>,
        error: Option<&str>,
        details_ref: Option<&str>,
    ) -> Result<(), ServiceError> {
        let connection = self.ledger.lock_connection()?;
        connection.execute("UPDATE service_ci_wait_operations SET status=CASE WHEN status='cancelling' THEN 'cancelled' ELSE ?2 END,observation_id=CASE WHEN status='cancelling' THEN NULL ELSE ?3 END,error_code=CASE WHEN status='cancelling' THEN 'cancelled' ELSE ?4 END,details_ref=?5,finished_at=?6 WHERE id=?1 AND status IN ('running','cancelling')",params![id.as_str(),status,observation_id,error,details_ref,now_ms()])?;
        Ok(())
    }

    fn ci_runtime(&self) -> Result<CiRuntime<'a, dyn CiProvider + Sync + 'a>, ServiceError> {
        let provider = self
            .ci_provider
            .ok_or(ServiceError::CiProviderUnavailable)?;
        CiRuntime::new(
            self.ledger,
            provider,
            self.ci_poll_interval,
            self.ci_api_timeout,
        )
        .map_err(ServiceError::from)
    }

    fn resolve_ci_target(
        &self,
        target: &CiServiceTarget,
        task_id: Option<&TaskId>,
    ) -> Result<(Option<TaskId>, CiQueryTarget, Option<String>), ServiceError> {
        let connection = self.ledger.lock_connection()?;
        resolve_ci_target_in_connection(&connection, target, task_id)
    }

    fn current_task_revision(&self, task_id: &TaskId) -> Result<u64, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let revision: Option<i64> = connection
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        revision
            .ok_or(ServiceError::TaskNotFound)?
            .try_into()
            .map_err(|_| ServiceError::InvalidStoredState)
    }

    fn find_publication_acceptance(
        &self,
        request_id: &str,
        digest: &str,
    ) -> Result<Option<ArtifactPublicationAcceptance>, ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let row: Option<(String, String, String, String, i64, String)> = connection
            .query_row(
                "SELECT publication.id,identity.publication_id,publication.task_id,
                        publication.request_digest,publication.revision,publication.status
                 FROM service_artifact_publication_operations publication
                 JOIN service_publication_ids identity ON identity.operation_id=publication.id
                 WHERE publication.request_id=?1",
                params![request_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(id, publication_id, task_id, stored_digest, revision, status)| {
                if stored_digest != digest {
                    return Err(ServiceError::IdempotencyConflict);
                }
                Ok(ArtifactPublicationAcceptance {
                    operation_id: OperationId::new(id),
                    publication_id: PublicationId::new(publication_id),
                    request_id: request_id.to_owned(),
                    task_id: TaskId::new(task_id),
                    revision: u64::try_from(revision)
                        .map_err(|_| ServiceError::InvalidStoredState)?,
                    status: ServiceOperationStatus::from_str(&status)?,
                })
            },
        )
        .transpose()
    }

    fn accept_artifact_publication(
        &self,
        request: &ArtifactPublicationRequest,
        permit: &ArtifactPublicationPermit,
        digest: &str,
    ) -> Result<(ArtifactPublicationAcceptance, bool), ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((id, publication_id, task, stored_digest, revision, status)) = tx
            .query_row(
                "SELECT publication.id,identity.publication_id,publication.task_id,
                        publication.request_digest,publication.revision,publication.status
                 FROM service_artifact_publication_operations publication
                 JOIN service_publication_ids identity ON identity.operation_id=publication.id
                 WHERE publication.request_id=?1",
                params![request.request_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
        {
            if stored_digest != digest {
                return Err(ServiceError::IdempotencyConflict);
            }
            let acceptance = ArtifactPublicationAcceptance {
                operation_id: OperationId::new(id),
                publication_id: PublicationId::new(publication_id),
                request_id: request.request_id.clone(),
                task_id: TaskId::new(task),
                revision: u64::try_from(revision).map_err(|_| ServiceError::InvalidStoredState)?,
                status: ServiceOperationStatus::from_str(&status)?,
            };
            tx.commit()?;
            return Ok((acceptance, false));
        }
        let task_state: String = tx
            .query_row(
                "SELECT state FROM tasks WHERE id=?1",
                params![request.task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ServiceError::TaskNotFound)?;
        if matches!(task_state.as_str(), "completed" | "failed" | "cancelled") {
            return Err(ServiceError::PolicyDenied("Task is closed"));
        }
        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![request.task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ServiceError::TaskNotFound)?;
        let revision = u64::try_from(revision).map_err(|_| ServiceError::InvalidStoredState)?;
        if revision != request.expected_revision {
            return Err(ServiceError::StaleRevision {
                expected: request.expected_revision,
                actual: revision,
            });
        }
        let active_attempt: Option<String> = tx.query_row(
            "SELECT id FROM service_operations WHERE task_id=?1 AND status IN ('accepted','running','recovery_required') ORDER BY rowid LIMIT 1",
            params![request.task_id.as_str()], |row| row.get(0),
        ).optional()?;
        if let Some(id) = active_attempt {
            return Err(ServiceError::Busy(OperationId::new(id)));
        }
        let active_publication: Option<String> = tx.query_row(
            "SELECT id FROM service_artifact_publication_operations WHERE task_id=?1 AND status IN ('accepted','running','recovery_required') ORDER BY rowid LIMIT 1",
            params![request.task_id.as_str()], |row| row.get(0),
        ).optional()?;
        if let Some(id) = active_publication {
            return Err(ServiceError::Busy(OperationId::new(id)));
        }
        let artifact_tree: Option<String> = tx.query_row(
            "SELECT tree_oid FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'",
            params![request.task_id.as_str(), request.artifact_id], |row| row.get(0),
        ).optional()?;
        if artifact_tree.as_deref() != Some(permit.tree_oid()) {
            return Err(ServiceError::Artifact(ArtifactError::RecoveryRequired));
        }
        let next_revision = revision
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or(ServiceError::InvalidStoredState)?;
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![request.task_id.as_str(), next_revision as i64],
        )?;
        let row_id: i64 = tx.query_row(
            "SELECT COALESCE(MAX(rowid),0)+1 FROM service_artifact_publication_operations",
            [],
            |row| row.get(0),
        )?;
        let accepted_at = now_ms();
        let operation_id = OperationId::new(format!("service-publication-{row_id}-{accepted_at}"));
        let publication_id = new_publication_id();
        let request_digest = digest;
        tx.execute(
            "INSERT INTO service_artifact_publication_operations(
                id,request_id,task_id,request_digest,artifact_id,tree_oid,base_commit,
                validation_id,decision_id,base_branch,head_branch,status,phase,
                accepted_revision,revision,accepted_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'accepted','scanned',?12,?13,?14)",
            params![
                operation_id.as_str(),
                request.request_id,
                request.task_id.as_str(),
                request_digest,
                request.artifact_id,
                permit.tree_oid(),
                permit.base_commit(),
                permit.validation_id(),
                permit.decision_id(),
                request.payload.base_branch(),
                request.payload.head_branch(),
                request.expected_revision as i64,
                next_revision as i64,
                accepted_at,
            ],
        )?;
        tx.execute(
            "INSERT INTO service_publication_ids(publication_id,operation_id) VALUES(?1,?2)",
            params![publication_id.as_str(), operation_id.as_str()],
        )?;
        tx.commit()?;
        Ok((
            ArtifactPublicationAcceptance {
                operation_id,
                publication_id,
                request_id: request.request_id.clone(),
                task_id: request.task_id.clone(),
                revision: next_revision,
                status: ServiceOperationStatus::Accepted,
            },
            true,
        ))
    }

    fn execute_artifact_publication(
        &self,
        operation_id: &OperationId,
        artifact: &crate::ArtifactRecord,
        permit: &ArtifactPublicationPermit,
        payload: &ArtifactPublicationPayload,
        gateway: &dyn ArtifactPublicationGateway,
    ) -> Result<(), ServiceError> {
        self.save_publication_phase(operation_id, ArtifactPublicationPhase::Committing, None)?;
        let commit_sha = match gateway.commit_tree(
            artifact.repository_root(),
            permit.tree_oid(),
            permit.base_commit(),
            "Publish validated Artifact",
            self.default_timeout,
        ) {
            Ok(sha) => sha,
            Err(_) => {
                self.finish_publication(
                    operation_id,
                    ServiceOperationStatus::Failed,
                    "publication_commit_failed",
                    None,
                    None,
                )?;
                return Err(ServiceError::PublicationFailed);
            }
        };
        if !valid_git_oid(&commit_sha)
            || !commit_matches_tree_and_base(
                artifact.repository_root(),
                &commit_sha,
                permit.tree_oid(),
                permit.base_commit(),
                self.default_timeout,
            )
        {
            self.finish_publication(
                operation_id,
                ServiceOperationStatus::Failed,
                "publication_commit_mismatch",
                None,
                None,
            )?;
            return Err(ServiceError::PublicationFailed);
        }
        self.save_publication_phase(
            operation_id,
            ArtifactPublicationPhase::Committed,
            Some(&commit_sha),
        )?;

        self.save_publication_phase(
            operation_id,
            ArtifactPublicationPhase::Pushing,
            Some(&commit_sha),
        )?;
        if gateway
            .push_commit(
                artifact.repository_root(),
                &commit_sha,
                payload.head_branch(),
                self.default_timeout,
            )
            .is_err()
        {
            self.finish_publication(
                operation_id,
                ServiceOperationStatus::RecoveryRequired,
                "publication_push_ambiguous",
                Some(&commit_sha),
                None,
            )?;
            return Err(ServiceError::PublicationRecoveryRequired);
        }
        self.save_publication_phase(
            operation_id,
            ArtifactPublicationPhase::Pushed,
            Some(&commit_sha),
        )?;

        self.save_publication_phase(
            operation_id,
            ArtifactPublicationPhase::CreatingDraft,
            Some(&commit_sha),
        )?;
        let pull_request = match gateway.create_or_find_draft_pull_request(
            artifact.repository_root(),
            payload,
            &commit_sha,
            self.default_timeout,
        ) {
            Ok(pull_request) => pull_request,
            Err(_) => {
                self.finish_publication(
                    operation_id,
                    ServiceOperationStatus::RecoveryRequired,
                    "publication_pr_ambiguous",
                    Some(&commit_sha),
                    None,
                )?;
                return Err(ServiceError::PublicationRecoveryRequired);
            }
        };
        if !pull_request.is_draft()
            || pull_request.head_sha() != commit_sha
            || pull_request.head_branch() != payload.head_branch()
            || pull_request.base_branch() != payload.base_branch()
            || pull_request.number() == 0
            || !pull_request.url().starts_with("https://")
        {
            self.finish_publication(
                operation_id,
                ServiceOperationStatus::RecoveryRequired,
                "publication_pr_mismatch",
                Some(&commit_sha),
                None,
            )?;
            return Err(ServiceError::PublicationRecoveryRequired);
        }
        self.finish_publication(
            operation_id,
            ServiceOperationStatus::Completed,
            "",
            Some(&commit_sha),
            Some(&pull_request),
        )?;
        Ok(())
    }

    fn set_publication_running(
        &self,
        operation_id: &OperationId,
        phase: ArtifactPublicationPhase,
    ) -> Result<bool, ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE service_artifact_publication_operations SET status='running',phase=?2,started_at=?3
             WHERE id=?1 AND status='accepted'",
            params![operation_id.as_str(), phase.as_str(), now_ms()],
        )?;
        if changed == 0 {
            tx.commit()?;
            return Ok(false);
        }
        if changed != 1 {
            return Err(ServiceError::PublicationRecoveryRequired);
        }
        tx.commit()?;
        Ok(true)
    }

    fn save_publication_phase(
        &self,
        operation_id: &OperationId,
        phase: ArtifactPublicationPhase,
        commit_sha: Option<&str>,
    ) -> Result<(), ServiceError> {
        let connection = self.ledger.lock_connection()?;
        let changed = connection.execute(
            "UPDATE service_artifact_publication_operations SET phase=?2,commit_sha=COALESCE(?3,commit_sha)
             WHERE id=?1 AND status='running'",
            params![operation_id.as_str(), phase.as_str(), commit_sha],
        )?;
        if changed != 1 {
            return Err(ServiceError::PublicationRecoveryRequired);
        }
        Ok(())
    }

    fn finish_publication(
        &self,
        operation_id: &OperationId,
        status: ServiceOperationStatus,
        error_code: &str,
        commit_sha: Option<&str>,
        pull_request: Option<&DraftPullRequest>,
    ) -> Result<(), ServiceError> {
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task: String = tx
            .query_row(
                "SELECT task_id FROM service_artifact_publication_operations WHERE id=?1",
                params![operation_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ServiceError::OperationNotFound)?;
        let phase = match status {
            ServiceOperationStatus::Completed => ArtifactPublicationPhase::Published,
            ServiceOperationStatus::Failed => ArtifactPublicationPhase::Failed,
            ServiceOperationStatus::RecoveryRequired => ArtifactPublicationPhase::RecoveryRequired,
            _ => return Err(ServiceError::InvalidStoredState),
        };
        let next_revision: i64 = tx
            .query_row(
                "SELECT revision+1 FROM service_task_revisions WHERE task_id=?1",
                params![task],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(ServiceError::TaskNotFound)?;
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![task, next_revision],
        )?;
        tx.execute(
            "UPDATE service_artifact_publication_operations SET status=?2,phase=?3,revision=?4,
                error_code=?5,commit_sha=COALESCE(?6,commit_sha),pull_request_number=?7,
                pull_request_url=?8,is_draft=?9,finished_at=?10 WHERE id=?1 AND status='running'",
            params![
                operation_id.as_str(),
                status.as_str(),
                phase.as_str(),
                next_revision,
                if error_code.is_empty() {
                    None
                } else {
                    Some(error_code)
                },
                commit_sha,
                pull_request.map(|pr| pr.number() as i64),
                pull_request.map(DraftPullRequest::url),
                pull_request.map(|pr| i64::from(pr.is_draft())),
                now_ms(),
            ],
        )?;
        tx.commit()?;
        Ok(())
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
        let pending_publications = {
            let mut statement = tx.prepare(
                "SELECT id,task_id,status FROM service_artifact_publication_operations
                 WHERE status IN ('accepted','running') ORDER BY rowid",
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
        for (operation_id, task_text, status) in pending_publications {
            let task_id = TaskId::new(task_text);
            match status.as_str() {
                "accepted" => {
                    tx.execute(
                        "UPDATE service_artifact_publication_operations
                         SET status='failed',phase='failed',
                             error_code='interrupted_before_start',finished_at=?2,revision=revision+1
                         WHERE id=?1 AND status='accepted'",
                        params![operation_id, now_ms()],
                    )?;
                }
                "running" => {
                    tx.execute(
                        "UPDATE service_artifact_publication_operations
                         SET status='recovery_required',phase='recovery_required',
                             error_code='interrupted',finished_at=?2,revision=revision+1
                         WHERE id=?1 AND status='running'",
                        params![operation_id, now_ms()],
                    )?;
                }
                _ => return Err(ServiceError::InvalidStoredState),
            }
            bump_revision(&tx, &task_id)?;
            recovered.push(OperationId::new(operation_id));
        }
        let pending_ci_waits = {
            let mut statement = tx.prepare("SELECT id FROM service_ci_wait_operations WHERE status IN ('accepted','running','cancelling') ORDER BY rowid")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for operation_id in pending_ci_waits {
            tx.execute("UPDATE service_ci_wait_operations SET status='recovery_required',error_code='interrupted',finished_at=?2 WHERE id=?1 AND status IN ('accepted','running','cancelling')", params![operation_id, now_ms()])?;
            recovered.push(OperationId::new(operation_id));
        }
        let pending_validations = {
            let mut statement = tx.prepare("SELECT id,task_id,status FROM service_validation_operations WHERE status IN ('accepted','running') ORDER BY accepted_at,id")?;
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
        for (operation_id, task_text, status) in pending_validations {
            let (next_status, code) = match status.as_str() {
                "accepted" => ("failed", "interrupted"),
                "running" => ("recovery_required", "interrupted"),
                _ => return Err(ServiceError::InvalidStoredState),
            };
            tx.execute("UPDATE service_validation_operations SET status=?2,error_code=?3,finished_at=?4 WHERE id=?1", params![operation_id, next_status, code, now_ms()])?;
            bump_revision(&tx, &TaskId::new(task_text))?;
            recovered.push(OperationId::new(operation_id));
        }
        let pending_cancellations = {
            let mut statement=tx.prepare("SELECT id,task_id FROM service_cancellation_operations WHERE status IN ('accepted','running') ORDER BY accepted_at,id")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (operation_id, task_id) in pending_cancellations {
            tx.execute("UPDATE service_cancellation_operations SET status='recovery_required',error_code='interrupted',finished_at=?2 WHERE id=?1",params![operation_id,now_ms()])?;
            bump_revision(&tx, &TaskId::new(task_id))?;
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

fn publication_request_digest(request: &ArtifactPublicationRequest) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ai-dev-orchestrator-publication-v1\0");
    let expected_revision = request.expected_revision.to_string();
    for field in [
        request.request_id.as_bytes(),
        request.task_id.as_str().as_bytes(),
        expected_revision.as_bytes(),
        request.artifact_id.as_bytes(),
        request.validation_id.as_bytes(),
        request.decision_id.as_bytes(),
        request.payload.base_branch().as_bytes(),
        request.payload.head_branch().as_bytes(),
        request.payload.title().as_bytes(),
        request.payload.body().as_bytes(),
    ] {
        digest.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(field);
    }
    let bytes = digest.finalize();
    let mut hex = String::with_capacity(7 + bytes.len() * 2);
    hex.push_str("sha256:");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn valid_git_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.ends_with('.')
        && !branch.contains("..")
        && !branch.contains("@{")
        && branch
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
        && !branch
            .bytes()
            .any(|byte| byte <= b' ' || byte == 0x7f || b"~^:?*[\\".contains(&byte))
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn commit_matches_tree_and_base(
    repository: &Path,
    commit_sha: &str,
    expected_tree: &str,
    expected_base: &str,
    timeout: Duration,
) -> bool {
    if !valid_git_oid(commit_sha) {
        return false;
    }
    let output = match crate::process_runner::ProcessRunner.run(
        crate::process_runner::ProcessRequest::new("git")
            .arg("-C")
            .arg(repository.as_os_str().to_owned())
            .args(["show", "--no-patch", "--format=%T%n%P", commit_sha])
            .timeout(timeout),
    ) {
        Ok(output) if !output.output_truncated => output,
        _ => return false,
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return false;
    };
    let mut lines = text.lines();
    lines.next() == Some(expected_tree)
        && lines.next() == Some(expected_base)
        && lines.next().is_none()
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

fn ci_target_json(target: &CiServiceTarget) -> String {
    let value = match target {
        CiServiceTarget::Publication(id) => {
            serde_json::json!({"kind":"publication","id":id.as_str()})
        }
        CiServiceTarget::PullRequest { repository, number } => {
            serde_json::json!({"kind":"pull_request","repository":repository,"number":number})
        }
        CiServiceTarget::Commit { repository, sha } => {
            serde_json::json!({"kind":"commit","repository":repository,"sha":sha})
        }
    };
    value.to_string()
}

type StoredPublicationCiTarget = (
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

fn resolve_ci_target_in_connection(
    connection: &rusqlite::Connection,
    target: &CiServiceTarget,
    task_id: Option<&TaskId>,
) -> Result<(Option<TaskId>, CiQueryTarget, Option<String>), ServiceError> {
    match target {
        CiServiceTarget::PullRequest { repository, number } => Ok((
            task_id.cloned(),
            CiQueryTarget::PullRequest {
                repository: repository.clone(),
                number: *number,
                expected_head_sha: None,
            },
            None,
        )),
        CiServiceTarget::Commit { repository, sha } => Ok((
            task_id.cloned(),
            CiQueryTarget::Commit {
                repository: repository.clone(),
                sha: sha.clone(),
            },
            None,
        )),
        CiServiceTarget::Publication(publication_id) => {
            let row: Option<StoredPublicationCiTarget> = connection.query_row(
                "SELECT publication.task_id,publication.status,publication.phase,publication.commit_sha,publication.pull_request_number,publication.pull_request_url,publication.is_draft
                 FROM service_publication_ids identity JOIN service_artifact_publication_operations publication ON publication.id=identity.operation_id
                 WHERE identity.publication_id=?1",
                params![publication_id.as_str()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
            ).optional()?;
            let (saved_task, status, phase, sha, number, url, draft) =
                row.ok_or(ServiceError::InvalidRequest("publication_id was not found"))?;
            if status != "completed" || phase != "published" || draft != Some(1) {
                return Err(ServiceError::PolicyDenied(
                    "CI requires a completed Publication operation",
                ));
            }
            let saved_task = TaskId::new(saved_task);
            if task_id.is_some_and(|task_id| task_id != &saved_task) {
                return Err(ServiceError::PolicyDenied(
                    "Publication operation belongs to a different Task",
                ));
            }
            let sha = sha.ok_or(ServiceError::InvalidStoredState)?;
            let number = u64::try_from(number.ok_or(ServiceError::InvalidStoredState)?)
                .map_err(|_| ServiceError::InvalidStoredState)?;
            let url = url.ok_or(ServiceError::InvalidStoredState)?;
            let repository = repository_from_pull_request_url(&url, number)
                .ok_or(ServiceError::InvalidStoredState)?;
            Ok((
                Some(saved_task),
                CiQueryTarget::PullRequest {
                    repository,
                    number,
                    expected_head_sha: Some(sha),
                },
                Some("github.com".to_owned()),
            ))
        }
    }
}

fn parse_ci_target(value: &str) -> Result<CiServiceTarget, ServiceError> {
    let value: serde_json::Value =
        serde_json::from_str(value).map_err(|_| ServiceError::InvalidStoredState)?;
    match value.get("kind").and_then(serde_json::Value::as_str) {
        Some("publication") => Ok(CiServiceTarget::Publication(PublicationId::new(
            value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or(ServiceError::InvalidStoredState)?,
        ))),
        Some("pull_request") => Ok(CiServiceTarget::PullRequest {
            repository: value
                .get("repository")
                .and_then(serde_json::Value::as_str)
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
            number: value
                .get("number")
                .and_then(serde_json::Value::as_u64)
                .ok_or(ServiceError::InvalidStoredState)?,
        }),
        Some("commit") => Ok(CiServiceTarget::Commit {
            repository: value
                .get("repository")
                .and_then(serde_json::Value::as_str)
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
            sha: value
                .get("sha")
                .and_then(serde_json::Value::as_str)
                .ok_or(ServiceError::InvalidStoredState)?
                .to_owned(),
        }),
        _ => Err(ServiceError::InvalidStoredState),
    }
}

fn new_publication_id() -> PublicationId {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_PUBLICATION_ID.fetch_add(1, Ordering::Relaxed);
    PublicationId::new(format!(
        "publication-v1-{time:x}-{:x}-{sequence:x}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{Child, Command},
        sync::{
            Arc, Barrier, Condvar, Mutex,
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        },
        thread,
        time::Instant,
    };

    use super::*;
    use crate::{
        AgentProvider, AgentResult, CiAggregateState, CiCheckDetailState, CiCheckSource,
        CiProviderError, CiProviderSnapshot, CiTarget, ProviderError, ProviderRegistry,
        ProviderResult, PublicationGatewayError, RawCiCheck, RequiredCheck, RequiredCheckSet,
        RequiredCheckSetSource, SecretScanError, Task, UsageCost,
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

    #[test]
    fn task_create_persists_pending_task_and_scoped_idempotency() {
        let repo = Repo::new();
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let providers = ProviderRegistry::new();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let request = TaskCreateRequest::new(
            "create-1",
            TaskSource::Manual,
            "Title",
            "Description",
            vec!["keep behavior".into()],
            None,
        );

        let first = service.create_task("caller-a", &request).unwrap();
        assert_eq!(first.revision(), 0);
        assert_eq!(first.state(), TaskState::Pending);
        assert_eq!(first.title(), "Title");
        assert_eq!(first.constraints(), &["keep behavior"]);
        let mut task = ledger.get_task(first.task_id()).unwrap().unwrap();
        assert_eq!(task.state(), TaskState::Pending);
        assert_eq!(task.role().as_str(), "unspecified");
        assert_eq!(task.description(), "Description");
        task.start().unwrap();
        ledger.save_task(&task).unwrap();

        let replay = service.create_task("caller-a", &request).unwrap();
        assert_eq!(replay, first);
        assert!(matches!(
            service.create_task(
                "caller-a",
                &TaskCreateRequest::new(
                    "create-1",
                    TaskSource::Manual,
                    "Changed",
                    "Description",
                    vec!["keep behavior".into()],
                    None
                )
            ),
            Err(ServiceError::IdempotencyConflict)
        ));
        let other_caller = service.create_task("caller-b", &request).unwrap();
        assert_ne!(other_caller.task_id(), first.task_id());
    }

    #[test]
    fn task_create_validates_issue_shape_before_persisting() {
        let repo = Repo::new();
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let providers = ProviderRegistry::new();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let missing_issue = TaskCreateRequest::new(
            "bad-1",
            TaskSource::Issue,
            "Title",
            "Description",
            vec![],
            None,
        );
        assert!(matches!(
            service.create_task("caller", &missing_issue),
            Err(ServiceError::InvalidRequest(_))
        ));
        let invalid_uri = TaskCreateRequest::new(
            "bad-2",
            TaskSource::Issue,
            "Title",
            "Description",
            vec![],
            Some(TaskIssueSnapshot {
                url: "not-a-uri".into(),
                number: 1,
                title: "Issue".into(),
                body: "Body".into(),
            }),
        );
        assert!(matches!(
            service.create_task("caller", &invalid_uri),
            Err(ServiceError::InvalidRequest(_))
        ));
        let empty_scheme = TaskCreateRequest::new(
            "bad-3",
            TaskSource::Issue,
            "Title",
            "Description",
            vec![],
            Some(TaskIssueSnapshot {
                url: ":rest".into(),
                number: 1,
                title: "Issue".into(),
                body: "Body".into(),
            }),
        );
        assert!(matches!(
            service.create_task("caller", &empty_scheme),
            Err(ServiceError::InvalidRequest(_))
        ));
        let valid_manual = TaskCreateRequest::new(
            "bad-4",
            TaskSource::Manual,
            "Title",
            "Description",
            vec![],
            None,
        );
        assert!(matches!(
            service.create_task("", &valid_manual),
            Err(ServiceError::InvalidRequest(_))
        ));
        let connection = ledger.lock_connection().unwrap();
        let task_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(task_count, 0);
    }

    #[test]
    fn task_create_replay_survives_reopening_the_ledger() {
        let repo = Repo::new();
        let database = repo.0.join("ledger.sqlite3");
        let providers = ProviderRegistry::new();
        let request = TaskCreateRequest::new(
            "durable-1",
            TaskSource::Issue,
            "Snapshot title",
            "Snapshot description",
            vec!["constraint".into()],
            Some(TaskIssueSnapshot {
                url: "https://example.test/issues/4".into(),
                number: 4,
                title: "Issue title".into(),
                body: "Issue body".into(),
            }),
        );
        let first = {
            let ledger = SqliteExecutionLedger::open(&database).unwrap();
            let workspace = WorkspaceManager::new(&repo.0).unwrap();
            let service =
                OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                    .unwrap();
            service.create_task("durable-caller", &request).unwrap()
        };
        let reopened = SqliteExecutionLedger::open(&database).unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let service = OperationService::new(
            &reopened,
            &workspace,
            &providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap();
        let replay = service.create_task("durable-caller", &request).unwrap();
        assert_eq!(replay, first);
        assert_eq!(replay.issue(), request.issue.as_ref());
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

    struct FakeSecretScanner {
        artifact_result: Result<SecretScanResult, SecretScanError>,
        payload_result: Result<SecretScanResult, SecretScanError>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl SecretScanner for FakeSecretScanner {
        fn scan_artifact_tree(
            &self,
            _repository: &Path,
            _tree_oid: &str,
        ) -> Result<SecretScanResult, SecretScanError> {
            self.events.lock().unwrap().push("scan_artifact");
            self.artifact_result
        }

        fn scan_publication_payload(
            &self,
            _payload: &ArtifactPublicationPayload,
        ) -> Result<SecretScanResult, SecretScanError> {
            self.events.lock().unwrap().push("scan_payload");
            self.payload_result
        }
    }

    struct BlockingSecretScanner {
        artifact_scans: AtomicUsize,
        entered: Barrier,
        release: Barrier,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl SecretScanner for BlockingSecretScanner {
        fn scan_artifact_tree(
            &self,
            _repository: &Path,
            _tree_oid: &str,
        ) -> Result<SecretScanResult, SecretScanError> {
            self.events.lock().unwrap().push("scan_artifact");
            if self.artifact_scans.fetch_add(1, Ordering::SeqCst) == 1 {
                self.entered.wait();
                self.release.wait();
            }
            Ok(SecretScanResult::Clean)
        }

        fn scan_publication_payload(
            &self,
            _payload: &ArtifactPublicationPayload,
        ) -> Result<SecretScanResult, SecretScanError> {
            self.events.lock().unwrap().push("scan_payload");
            Ok(SecretScanResult::Clean)
        }
    }

    struct CleanThenFindingScanner {
        payload_scans: AtomicUsize,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl SecretScanner for CleanThenFindingScanner {
        fn scan_artifact_tree(
            &self,
            _repository: &Path,
            _tree_oid: &str,
        ) -> Result<SecretScanResult, SecretScanError> {
            self.events.lock().unwrap().push("scan_artifact");
            Ok(SecretScanResult::Clean)
        }

        fn scan_publication_payload(
            &self,
            _payload: &ArtifactPublicationPayload,
        ) -> Result<SecretScanResult, SecretScanError> {
            self.events.lock().unwrap().push("scan_payload");
            if self.payload_scans.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(SecretScanResult::Clean)
            } else {
                Ok(SecretScanResult::Findings)
            }
        }
    }

    struct FakeArtifactPublicationGateway {
        repository: PathBuf,
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_push: bool,
        fail_pull_request: bool,
    }

    impl ArtifactPublicationGateway for FakeArtifactPublicationGateway {
        fn supports_sensitive_stdin_payload(&self) -> bool {
            true
        }

        fn commit_tree(
            &self,
            _repository: &Path,
            tree_oid: &str,
            base_commit: &str,
            message: &str,
            _timeout: Duration,
        ) -> Result<String, PublicationGatewayError> {
            self.events.lock().unwrap().push("commit");
            Ok(git(
                &self.repository,
                &["commit-tree", tree_oid, "-p", base_commit, "-m", message],
            ))
        }

        fn push_commit(
            &self,
            _repository: &Path,
            _commit_sha: &str,
            _head_branch: &str,
            _timeout: Duration,
        ) -> Result<(), PublicationGatewayError> {
            self.events.lock().unwrap().push("push");
            if self.fail_push {
                Err(PublicationGatewayError::CommandFailed)
            } else {
                Ok(())
            }
        }

        fn create_or_find_draft_pull_request(
            &self,
            _repository: &Path,
            payload: &ArtifactPublicationPayload,
            commit_sha: &str,
            _timeout: Duration,
        ) -> Result<DraftPullRequest, PublicationGatewayError> {
            self.events.lock().unwrap().push("draft_pr");
            if self.fail_pull_request {
                return Err(PublicationGatewayError::CommandFailed);
            }
            Ok(DraftPullRequest::new(
                17,
                "https://github.com/owner/repo/pull/17",
                true,
                commit_sha,
                payload.head_branch(),
                payload.base_branch(),
            ))
        }
    }

    struct UnsupportedStdinPublicationGateway {
        calls: AtomicUsize,
        supports_stdin: AtomicBool,
    }

    impl ArtifactPublicationGateway for UnsupportedStdinPublicationGateway {
        fn supports_sensitive_stdin_payload(&self) -> bool {
            self.supports_stdin.load(Ordering::SeqCst)
        }

        fn commit_tree(
            &self,
            _repository: &Path,
            _tree_oid: &str,
            _base_commit: &str,
            _message: &str,
            _timeout: Duration,
        ) -> Result<String, PublicationGatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(PublicationGatewayError::CommandFailed)
        }

        fn push_commit(
            &self,
            _repository: &Path,
            _commit_sha: &str,
            _head_branch: &str,
            _timeout: Duration,
        ) -> Result<(), PublicationGatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(PublicationGatewayError::CommandFailed)
        }

        fn create_or_find_draft_pull_request(
            &self,
            _repository: &Path,
            _payload: &ArtifactPublicationPayload,
            _commit_sha: &str,
            _timeout: Duration,
        ) -> Result<DraftPullRequest, PublicationGatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(PublicationGatewayError::CommandFailed)
        }
    }

    struct FakeCiProvider {
        snapshot: crate::CiProviderSnapshot,
        queries: Mutex<Vec<CiQueryTarget>>,
        hosts: Mutex<Vec<String>>,
    }

    struct BlockingCiProvider {
        snapshot: crate::CiProviderSnapshot,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    struct SyncTestResolver;

    impl crate::ProviderResolver for SyncTestResolver {
        fn resolve(
            &self,
            provider: &ProviderRef,
        ) -> Result<&dyn AgentProvider, crate::ProviderResolutionError> {
            Err(crate::ProviderResolutionError::UnknownProvider {
                provider: provider.clone(),
            })
        }
    }

    impl crate::CiProvider for BlockingCiProvider {
        fn observe(
            &self,
            _target: &CiQueryTarget,
            _timeout: Duration,
        ) -> Result<crate::CiProviderSnapshot, CiProviderError> {
            self.entered.wait();
            self.release.wait();
            Ok(self.snapshot.clone())
        }
    }

    impl crate::CiProvider for FakeCiProvider {
        fn observe(
            &self,
            target: &CiQueryTarget,
            _timeout: Duration,
        ) -> Result<crate::CiProviderSnapshot, CiProviderError> {
            self.queries.lock().unwrap().push(target.clone());
            Ok(self.snapshot.clone())
        }

        fn observe_on_host(
            &self,
            target: &CiQueryTarget,
            timeout: Duration,
            host: &str,
        ) -> Result<crate::CiProviderSnapshot, CiProviderError> {
            self.hosts.lock().unwrap().push(host.to_owned());
            self.observe(target, timeout)
        }
    }

    fn ci_provider_snapshot(
        repository: &str,
        pull_request_number: Option<u64>,
        head_sha: &str,
    ) -> crate::CiProviderSnapshot {
        CiProviderSnapshot {
            target: CiTarget::new(repository, pull_request_number, head_sha),
            required_checks: RequiredCheckSet::known_from(
                vec![RequiredCheck::new("build", Some(7))],
                RequiredCheckSetSource::TrustedConfiguration,
                123,
            ),
            checks: vec![RawCiCheck {
                name: "build".into(),
                detail_state: CiCheckDetailState::Passed,
                url: Some("https://example.test/check".into()),
                completed_at: Some("2026-01-01T00:00:00Z".into()),
                app_id: Some(7),
                source: CiCheckSource::GithubCheckRuns,
            }],
            check_runs_available: true,
            commit_statuses_available: true,
            observed_at_ms: 123,
        }
    }

    fn publish_fixture(
        fixture: &ArtifactPublicationFixture,
        request_id: &str,
    ) -> ArtifactPublicationSnapshot {
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: Arc::clone(&events),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events,
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(fixture, request_id);
        let acceptance = service.publish_artifact(&request).unwrap();
        assert!(
            acceptance
                .publication_id()
                .as_str()
                .starts_with("publication-v1-")
        );
        assert_ne!(
            acceptance.publication_id().as_str(),
            acceptance.operation_id().as_str()
        );
        service
            .run_artifact_publication(&acceptance, &request)
            .unwrap()
    }

    struct ArtifactPublicationFixture {
        repo: Repo,
        ledger: SqliteExecutionLedger,
        workspace: WorkspaceManager,
        providers: ProviderRegistry,
        task_id: TaskId,
        artifact_id: String,
        validation_id: String,
        decision_id: String,
        revision: u64,
        attempt_id: AttemptId,
    }

    fn artifact_publication_fixture() -> ArtifactPublicationFixture {
        artifact_publication_fixture_with_persistent_ledger(false)
    }

    fn artifact_publication_fixture_with_persistent_ledger(
        persistent_ledger: bool,
    ) -> ArtifactPublicationFixture {
        use crate::{CommandValidator, ValidationCheck};
        let repo = Repo::new();
        let ledger = if persistent_ledger {
            SqliteExecutionLedger::open(repo.0.join("service-ledger.sqlite")).unwrap()
        } else {
            SqliteExecutionLedger::open_in_memory().unwrap()
        };
        let task_id = TaskId::new("publication-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "publication task",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let mut providers = ProviderRegistry::new();
        providers.register(FakeProvider {
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
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "publication-source"))
            .unwrap();
        let completed = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();
        let artifact_id = completed.output_artifact_id().unwrap().to_owned();
        let validation = service
            .validate_artifact(
                &task_id,
                &artifact_id,
                task_revision(&ledger, &task_id),
                &CommandValidator::new([ValidationCheck::new("passes", "true")]),
            )
            .unwrap();
        let decision = service
            .record_artifact_decision(
                &task_id,
                &artifact_id,
                task_revision(&ledger, &task_id),
                CodexDecisionKind::Accepted,
                "Accepted for publication",
                &[("validation".into(), validation.id().into())],
            )
            .unwrap();
        let revision = task_revision(&ledger, &task_id);
        // The fixture models a completed prior process. Release its exclusive
        // run lock before the caller constructs a fresh Service over this Ledger.
        drop(service);
        ArtifactPublicationFixture {
            repo,
            ledger,
            workspace,
            providers,
            task_id,
            artifact_id,
            validation_id: validation.id().to_owned(),
            decision_id: decision.id().to_owned(),
            revision,
            attempt_id: accepted.attempt_id().clone(),
        }
    }

    fn task_revision(ledger: &SqliteExecutionLedger, task_id: &TaskId) -> u64 {
        ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap() as u64
    }

    fn publication_request(
        fixture: &ArtifactPublicationFixture,
        request_id: &str,
    ) -> ArtifactPublicationRequest {
        ArtifactPublicationRequest::new(
            request_id,
            fixture.task_id.clone(),
            fixture.revision,
            fixture.artifact_id.clone(),
            fixture.validation_id.clone(),
            fixture.decision_id.clone(),
            ArtifactPublicationPayload::new(
                "main",
                "codex/artifact-publication",
                "payload-title-private-marker",
                "payload-body-private-marker",
            ),
        )
    }

    struct AllowTrueValidationPolicy;

    impl ValidationPolicy for AllowTrueValidationPolicy {
        fn validate(
            &self,
            repository_root: &Path,
            _workspace: &Path,
            profile_id: Option<&str>,
            checks: &[ValidationCheckSpec],
        ) -> Result<ValidationResult, crate::ValidatorError> {
            use crate::{CommandValidator, ValidationCheck};
            assert_eq!(profile_id, None);
            assert!(!checks.is_empty());
            assert!(checks.iter().all(|check| check.command() == "true"));
            assert!(repository_root.is_dir());
            let checks = checks
                .iter()
                .map(|check| {
                    ValidationCheck::new(check.name(), check.command())
                        .args(check.args().iter().cloned())
                        .timeout(Duration::from_millis(check.timeout_ms()))
                })
                .collect::<Vec<_>>();
            CommandValidator::new(checks).validate(_workspace)
        }
    }

    #[test]
    fn validation_run_is_idempotent_async_and_exposes_saved_check_result() {
        let fixture = artifact_publication_fixture();
        let policy = AllowTrueValidationPolicy;
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_validation_policy(&policy);
        let request = ValidationRunRequest::new(
            "validation-request-1",
            fixture.task_id.clone(),
            fixture.revision,
            fixture.artifact_id.clone(),
            None,
            vec![ValidationCheckSpec::new(
                "fast-check",
                "true",
                Vec::new(),
                1000,
            )],
        );
        let accepted = service
            .accept_validation("validation-caller", &request)
            .unwrap();
        assert_eq!(accepted.status(), ServiceOperationStatus::Accepted);
        assert_eq!(accepted.revision(), fixture.revision + 1);
        assert_eq!(
            service
                .accept_validation("validation-caller", &request)
                .unwrap(),
            accepted
        );
        let completed = service
            .run_validation_operation(accepted.operation_id())
            .unwrap();
        assert_eq!(completed.status(), ServiceOperationStatus::Completed);
        let result = completed.result().unwrap();
        assert_eq!(result["state"], "passed");
        assert_eq!(result["artifact_id"], fixture.artifact_id);
        assert_eq!(result["checks"][0]["name"], "fast-check");
        assert_eq!(result["checks"][0]["state"], "passed");
        assert!(result["checks"][0]["diagnostic_ref"].is_null());
        assert!(matches!(
            service
                .get_operation_result(accepted.operation_id())
                .unwrap(),
            OperationGetResult::Validation(_)
        ));
    }

    #[test]
    fn task_finish_checks_artifact_decision_and_persists_idempotent_response_atomically() {
        let fixture = artifact_publication_fixture();
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap();
        let evidence = vec![("validation".to_owned(), fixture.validation_id.clone())];
        let first = service
            .finish_task_idempotent(
                "mcp-test-caller",
                "finish-1",
                &fixture.task_id,
                fixture.revision,
                &fixture.artifact_id,
                &fixture.decision_id,
                &evidence,
            )
            .unwrap();
        assert_eq!(first.revision(), fixture.revision + 1);
        assert_eq!(first.evidence(), evidence.as_slice());
        let replay = service
            .finish_task_idempotent(
                "mcp-test-caller",
                "finish-1",
                &fixture.task_id,
                fixture.revision,
                &fixture.artifact_id,
                &fixture.decision_id,
                &evidence,
            )
            .unwrap();
        assert_eq!(replay, first);
        assert!(matches!(
            service.finish_task_idempotent(
                "mcp-test-caller",
                "finish-1",
                &fixture.task_id,
                fixture.revision,
                &fixture.artifact_id,
                &fixture.decision_id,
                &[],
            ),
            Err(ServiceError::IdempotencyConflict)
        ));
        let (state, revision): (String, i64) = fixture.ledger.lock_connection().unwrap().query_row(
            "SELECT tasks.state,service_task_revisions.revision FROM tasks JOIN service_task_revisions ON tasks.id=service_task_revisions.task_id WHERE tasks.id=?1",
            params![fixture.task_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(state, "completed");
        assert_eq!(revision, i64::try_from(first.revision()).unwrap());
    }

    #[derive(Default)]
    struct FailingValidator {
        workspace: std::sync::Mutex<Option<PathBuf>>,
    }

    #[derive(Default)]
    struct TrackingValidator {
        workspace: std::sync::Mutex<Option<PathBuf>>,
    }

    impl crate::Validator for TrackingValidator {
        fn validate(
            &self,
            workspace: &std::path::Path,
        ) -> Result<ValidationResult, crate::ValidatorError> {
            *self.workspace.lock().unwrap() = Some(workspace.to_owned());
            Ok(ValidationResult::from_checks(
                "passed",
                [crate::ValidationCheckResult::new(
                    "passes",
                    true,
                    Some(0),
                    "",
                )],
            ))
        }
    }

    #[derive(Default)]
    struct IgnoredFileValidator {
        workspace: std::sync::Mutex<Option<PathBuf>>,
    }

    impl crate::Validator for IgnoredFileValidator {
        fn validate(
            &self,
            workspace: &std::path::Path,
        ) -> Result<ValidationResult, crate::ValidatorError> {
            *self.workspace.lock().unwrap() = Some(workspace.to_owned());
            fs::write(workspace.join("ignored-validation.txt"), "not in Artifact")
                .expect("write ignored validation output");
            Ok(ValidationResult::from_checks(
                "passed",
                [crate::ValidationCheckResult::new(
                    "passes",
                    true,
                    Some(0),
                    "",
                )],
            ))
        }
    }

    impl crate::Validator for FailingValidator {
        fn validate(
            &self,
            workspace: &std::path::Path,
        ) -> Result<ValidationResult, crate::ValidatorError> {
            *self.workspace.lock().unwrap() = Some(workspace.to_owned());
            Err(crate::ValidatorError::NoChecksConfigured)
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
    fn artifact_validation_gate_uses_an_immutable_snapshot_and_exact_decision() {
        use crate::{CommandValidator, ValidationCheck};

        let repo = Repo::new();
        fs::write(repo.0.join(".gitignore"), "ignored-validation.txt\n").unwrap();
        git(&repo.0, &["add", ".gitignore"]);
        git(
            &repo.0,
            &["commit", "-m", "ignore validation fixture output"],
        );
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task_id = TaskId::new("artifact-evidence-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "task description",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let workspace = WorkspaceManager::new(&repo.0).unwrap();
        let mut providers = ProviderRegistry::new();
        providers.register(FakeProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            availability_checks: Arc::new(AtomicUsize::new(0)),
            fail: false,
            unknown_interrupt: false,
            provider_failure: None,
            execute_delay: Duration::ZERO,
            reference: ProviderRef::new("fake"),
            write_output: Some(("artifact-new-file.txt".into(), "artifact content".into())),
            write_ignored: None,
            write_gitignore: None,
            require_file: None,
        });
        let service =
            OperationService::new(&ledger, &workspace, &providers, 3, Duration::from_secs(30))
                .unwrap();
        assert!(matches!(
            service.validate_artifact(
                &TaskId::new("missing-task"),
                "missing-artifact",
                0,
                &CommandValidator::new([ValidationCheck::new("passes", "true")]),
            ),
            Err(ServiceError::TaskNotFound)
        ));
        assert!(matches!(
            service.validate_artifact(
                &task_id,
                "missing-artifact",
                99,
                &CommandValidator::new([ValidationCheck::new("passes", "true")]),
            ),
            Err(ServiceError::StaleRevision {
                expected: 99,
                actual: 0
            })
        ));
        let accepted = service
            .submit_attempt(&request(&repo, &task_id, 0, "artifact-evidence-run"))
            .unwrap();
        let completed = service
            .run(accepted.operation_id(), CancellationToken::new())
            .unwrap();
        let artifact_id = completed.output_artifact_id().unwrap().to_owned();
        let artifact_tree = ArtifactManager::new(&workspace, &ledger)
            .verify_input(&task_id, &artifact_id)
            .unwrap()
            .tree_oid()
            .to_owned();
        let revision = || -> u64 {
            ledger
                .lock_connection()
                .unwrap()
                .query_row(
                    "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                    params![task_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap() as u64
        };
        let failing_validator = FailingValidator::default();
        assert!(matches!(
            service.validate_artifact(&task_id, &artifact_id, revision(), &failing_validator,),
            Err(ServiceError::ValidationFailed)
        ));
        let failed_workspace = failing_validator.workspace.lock().unwrap().clone().unwrap();
        assert!(!failed_workspace.exists());
        let validation_count: i64 = ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM artifact_validations WHERE task_id=?1 AND artifact_id=?2",
                params![task_id.as_str(), artifact_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(validation_count, 0);
        assert!(
            git(&repo.0, &["ls-tree", "-r", "--name-only", &artifact_tree])
                .contains("artifact-new-file.txt")
        );
        let ignored_validator = IgnoredFileValidator::default();
        let ignored_result =
            service.validate_artifact(&task_id, &artifact_id, revision(), &ignored_validator);
        let ignored_path = match ignored_result {
            Err(ServiceError::Artifact(ArtifactError::WorkspaceRetained { path, .. })) => path,
            other => panic!("ignored validation output must be retained: {other:?}"),
        };
        assert!(ignored_path.exists());
        assert!(ignored_path.join("ignored-validation.txt").exists());
        let validation_count: i64 = ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM artifact_validations WHERE task_id=?1 AND artifact_id=?2",
                params![task_id.as_str(), artifact_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(validation_count, 0);
        let ignored_path_text = ignored_path.to_string_lossy().into_owned();
        git(
            &repo.0,
            &["worktree", "remove", "--force", &ignored_path_text],
        );
        let tracking_validator = TrackingValidator::default();
        let validation = service
            .validate_artifact(&task_id, &artifact_id, revision(), &tracking_validator)
            .unwrap();
        let successful_workspace = tracking_validator
            .workspace
            .lock()
            .unwrap()
            .clone()
            .unwrap();
        assert!(!successful_workspace.exists());
        assert!(validation.passed());
        assert_eq!(validation.revision(), revision());
        let other_validation = service
            .validate_artifact(
                &task_id,
                &artifact_id,
                revision(),
                &CommandValidator::new([ValidationCheck::new("passes-again", "true")]),
            )
            .unwrap();
        let decision_expected_revision = revision();
        let decision_id = service
            .record_artifact_decision_idempotent(
                "mcp-test-caller",
                "decision-idempotency-1",
                &task_id,
                &artifact_id,
                decision_expected_revision,
                CodexDecisionKind::Accepted,
                "The supervisor accepted this artifact.",
                &[("validation".into(), validation.id().into())],
            )
            .unwrap();
        let replayed_decision = service
            .record_artifact_decision_idempotent(
                "mcp-test-caller",
                "decision-idempotency-1",
                &task_id,
                &artifact_id,
                decision_expected_revision,
                CodexDecisionKind::Accepted,
                "The supervisor accepted this artifact.",
                &[("validation".into(), validation.id().into())],
            )
            .unwrap();
        assert_eq!(replayed_decision.id(), decision_id.id());
        assert!(matches!(
            service.record_artifact_decision_idempotent(
                "mcp-test-caller",
                "decision-idempotency-1",
                &task_id,
                &artifact_id,
                decision_expected_revision,
                CodexDecisionKind::Rejected,
                "different payload",
                &[("validation".into(), validation.id().into())],
            ),
            Err(ServiceError::IdempotencyConflict)
        ));
        assert_eq!(decision_id.artifact_id(), artifact_id);
        assert_eq!(decision_id.tree_oid(), validation.tree_oid());
        assert_eq!(decision_id.revision(), revision());
        let permit = service
            .require_artifact_publication_evidence(
                &task_id,
                &artifact_id,
                validation.id(),
                decision_id.id(),
                revision(),
            )
            .unwrap();
        assert_eq!(permit.artifact_id(), artifact_id);
        assert_eq!(permit.tree_oid(), validation.tree_oid());
        assert!(matches!(
            service.require_artifact_publication_evidence(
                &task_id,
                &artifact_id,
                other_validation.id(),
                decision_id.id(),
                revision(),
            ),
            Err(ServiceError::Artifact(ArtifactError::Invalid(_)))
        ));

        let rejected_decision = service
            .record_artifact_decision(
                &task_id,
                &artifact_id,
                revision(),
                CodexDecisionKind::Rejected,
                "The accepted decision was superseded.",
                &[("validation".into(), validation.id().into())],
            )
            .unwrap();
        assert!(matches!(
            service.require_artifact_publication_evidence(
                &task_id,
                &artifact_id,
                validation.id(),
                decision_id.id(),
                revision(),
            ),
            Err(ServiceError::Artifact(ArtifactError::Invalid(_)))
        ));
        assert_eq!(rejected_decision.revision(), revision());

        let rejected = service.validate_artifact(
            &task_id,
            &artifact_id,
            revision(),
            &CommandValidator::new([
                ValidationCheck::new("mutates", "sh").args(["-c", "printf changed >> README.md"])
            ]),
        );
        let retained_path = match rejected {
            Err(ServiceError::Artifact(ArtifactError::WorkspaceRetained { path, .. })) => path,
            other => panic!("mutated validation worktree should be retained: {other:?}"),
        };
        assert!(retained_path.exists());
        let validation_count: i64 = ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM artifact_validations WHERE task_id=?1 AND artifact_id=?2",
                params![task_id.as_str(), artifact_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(validation_count, 2);
        let retained_path_text = retained_path.to_string_lossy().into_owned();
        git(
            &repo.0,
            &["worktree", "remove", "--force", &retained_path_text],
        );
        cleanup_fixture_worktree(&repo, &workspace, &task_id, completed.attempt_id());
    }

    #[test]
    fn publication_rejects_unsupported_sensitive_stdin_before_recording_or_scanning() {
        let fixture = artifact_publication_fixture();
        let scanner_events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: scanner_events.clone(),
        };
        let gateway = UnsupportedStdinPublicationGateway {
            calls: AtomicUsize::new(0),
            supports_stdin: AtomicBool::new(false),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "unsupported-sensitive-stdin");
        assert!(matches!(
            service.publish_artifact(&request),
            Err(ServiceError::PolicyDenied(
                "publication gateway cannot safely send title/body on this platform"
            ))
        ));
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
        assert!(scanner_events.lock().unwrap().is_empty());
        let operation_count: i64 = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM service_artifact_publication_operations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(operation_count, 0);
        assert!(
            service
                .require_artifact_publication_evidence(
                    &fixture.task_id,
                    &fixture.artifact_id,
                    &fixture.validation_id,
                    &fixture.decision_id,
                    fixture.revision,
                )
                .is_ok()
        );
    }

    #[test]
    fn publication_replay_precedes_current_configuration_checks_but_run_rechecks_capability() {
        let fixture = artifact_publication_fixture();
        let scanner_events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: scanner_events,
        };
        let gateway = UnsupportedStdinPublicationGateway {
            calls: AtomicUsize::new(0),
            supports_stdin: AtomicBool::new(true),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "capability-changed-after-acceptance");
        let acceptance = service.publish_artifact(&request).unwrap();
        gateway.supports_stdin.store(false, Ordering::SeqCst);

        let replay_service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap();
        let replayed = replay_service.publish_artifact(&request).unwrap();
        assert_eq!(replayed.operation_id(), acceptance.operation_id());
        assert_eq!(replayed.publication_id(), acceptance.publication_id());

        assert!(matches!(
            service.run_artifact_publication(&acceptance, &request),
            Err(ServiceError::PolicyDenied(
                "publication gateway cannot safely send title/body on this platform"
            ))
        ));
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
        let operation = service
            .get_artifact_publication_operation(acceptance.operation_id())
            .unwrap();
        assert_eq!(operation.state(), ServiceOperationStatus::Accepted);
        assert_eq!(operation.phase(), ArtifactPublicationPhase::Scanned);

        let new_request = publication_request(&fixture, "new-request-after-capability-change");
        assert!(matches!(
            replay_service.publish_artifact(&new_request),
            Err(ServiceError::PolicyDenied(
                "Artifact publication gateway is not configured"
            ))
        ));
    }

    #[test]
    fn publication_requires_a_scanner_before_recording_or_gateway_effects() {
        let fixture = artifact_publication_fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let providers = ProviderRegistry::new();
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_artifact_publication_gateway(&gateway);
        assert!(matches!(
            service.publish_artifact(&publication_request(&fixture, "scanner-required")),
            Err(ServiceError::PolicyDenied(
                "SecretScanner is not configured"
            ))
        ));
        assert!(events.lock().unwrap().is_empty());
        let count: i64 = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM service_artifact_publication_operations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn publication_rejects_identical_base_and_head_branches_before_scanning_or_effects() {
        let fixture = artifact_publication_fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = ArtifactPublicationRequest::new(
            "same-publication-branches",
            fixture.task_id.clone(),
            fixture.revision,
            fixture.artifact_id.clone(),
            fixture.validation_id.clone(),
            fixture.decision_id.clone(),
            ArtifactPublicationPayload::new("main", "main", "title", "body"),
        );

        assert!(matches!(
            service.publish_artifact(&request),
            Err(ServiceError::InvalidRequest(
                "publication base and head branches must differ"
            ))
        ));
        assert!(events.lock().unwrap().is_empty());
        let count: i64 = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM service_artifact_publication_operations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn publication_scans_before_effects_and_persists_exact_draft_evidence_without_payload_text() {
        let fixture = artifact_publication_fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "publish-artifact-once");
        let acceptance = service.publish_artifact(&request).unwrap();
        assert_eq!(acceptance.status(), ServiceOperationStatus::Accepted);
        let accepted_snapshot = service
            .get_artifact_publication_operation(acceptance.operation_id())
            .unwrap();
        assert_eq!(accepted_snapshot.state(), ServiceOperationStatus::Accepted);
        assert_eq!(accepted_snapshot.phase(), ArtifactPublicationPhase::Scanned);
        assert_eq!(*events.lock().unwrap(), ["scan_artifact", "scan_payload"]);
        let snapshot = service
            .run_artifact_publication(&acceptance, &request)
            .unwrap();
        assert_eq!(acceptance.publication_id(), snapshot.publication_id());
        assert_eq!(snapshot.state(), ServiceOperationStatus::Completed);
        assert_eq!(snapshot.phase(), ArtifactPublicationPhase::Published);
        assert_eq!(snapshot.artifact_id(), fixture.artifact_id);
        assert!(matches!(
            service.get_artifact_publication_by_id(&PublicationId::new(
                snapshot.operation_id().as_str()
            )),
            Err(ServiceError::InvalidRequest("publication_id was not found"))
        ));
        assert!(valid_git_oid(snapshot.commit_sha().unwrap()));
        let pull_request = snapshot.pull_request().unwrap();
        assert_eq!(pull_request.number(), 17);
        assert!(pull_request.is_draft());
        assert_eq!(pull_request.head_sha(), snapshot.commit_sha().unwrap());
        let commit_meta = git(
            &fixture.repo.0,
            &[
                "show",
                "--no-patch",
                "--format=%T%n%P",
                snapshot.commit_sha().unwrap(),
            ],
        );
        assert_eq!(commit_meta.lines().next(), Some(snapshot.tree_oid()));
        assert_eq!(
            commit_meta.lines().nth(1),
            Some(git(&fixture.repo.0, &["rev-parse", "HEAD"]).as_str())
        );
        assert_eq!(
            *events.lock().unwrap(),
            [
                "scan_artifact",
                "scan_payload",
                "scan_artifact",
                "scan_payload",
                "commit",
                "push",
                "draft_pr"
            ]
        );

        let duplicate = service.publish_artifact(&request).unwrap();
        assert_eq!(duplicate.operation_id(), acceptance.operation_id());
        assert_eq!(duplicate.publication_id(), acceptance.publication_id());
        assert_eq!(duplicate.status(), ServiceOperationStatus::Completed);
        assert_eq!(
            service
                .get_artifact_publication_by_id(acceptance.publication_id())
                .unwrap(),
            snapshot
        );
        assert_eq!(events.lock().unwrap().len(), 7);
        let mut changed = request.clone();
        changed.payload = ArtifactPublicationPayload::new(
            "main",
            "codex/artifact-publication",
            "payload-title-private-marker",
            "different-body",
        );
        assert!(matches!(
            service.publish_artifact(&changed),
            Err(ServiceError::IdempotencyConflict)
        ));
        assert!(matches!(
            service.run_artifact_publication(&acceptance, &changed),
            Err(ServiceError::IdempotencyConflict)
        ));

        let connection = fixture.ledger.lock_connection().unwrap();
        let columns = {
            let mut statement = connection
                .prepare("PRAGMA table_info(service_artifact_publication_operations)")
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(
            !columns
                .iter()
                .any(|column| column == "title" || column == "body")
        );
        let digest: String = connection
            .query_row(
                "SELECT request_digest FROM service_artifact_publication_operations WHERE id=?1",
                params![acceptance.operation_id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(digest.starts_with("sha256:") && digest.len() == 71);
        assert!(!digest.contains("payload-title-private-marker"));
        drop(connection);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn concurrent_publication_runs_share_one_claim_and_one_effect_sequence() {
        let fixture = artifact_publication_fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = BlockingSecretScanner {
            artifact_scans: AtomicUsize::new(0),
            entered: Barrier::new(2),
            release: Barrier::new(2),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "publication-parallel-run");
        let acceptance = service.publish_artifact(&request).unwrap();

        let ledger = &fixture.ledger;
        let workspace = &fixture.workspace;
        let scanner_ref = &scanner;
        let gateway_ref = &gateway;
        let first_acceptance = acceptance.clone();
        let first_request = request.clone();
        let (first, concurrent) = std::thread::scope(|scope| {
            let first = scope.spawn(move || {
                let first_providers = ProviderRegistry::new();
                let first_service = OperationService::new(
                    ledger,
                    workspace,
                    &first_providers,
                    3,
                    Duration::from_secs(30),
                )
                .unwrap()
                .with_secret_scanner(scanner_ref)
                .with_artifact_publication_gateway(gateway_ref);
                first_service.run_artifact_publication(&first_acceptance, &first_request)
            });
            scanner.entered.wait();
            let concurrent = service.run_artifact_publication(&acceptance, &request);
            scanner.release.wait();
            (first.join().unwrap(), concurrent)
        });
        let first = first.unwrap();
        let concurrent = concurrent.unwrap();
        assert_eq!(first.state(), ServiceOperationStatus::Completed);
        assert_eq!(concurrent.state(), ServiceOperationStatus::Running);
        assert_eq!(
            *events.lock().unwrap(),
            [
                "scan_artifact",
                "scan_payload",
                "scan_artifact",
                "scan_payload",
                "commit",
                "push",
                "draft_pr"
            ]
        );
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn publication_rescans_before_run_and_marks_changed_payload_rejected() {
        let fixture = artifact_publication_fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = CleanThenFindingScanner {
            payload_scans: AtomicUsize::new(0),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "payload-changed-after-acceptance");
        let acceptance = service.publish_artifact(&request).unwrap();
        assert_eq!(acceptance.status(), ServiceOperationStatus::Accepted);
        assert!(matches!(
            service.run_artifact_publication(&acceptance, &request),
            Err(ServiceError::PolicyDenied(
                "secret scan found sensitive content"
            ))
        ));
        let snapshot = service
            .get_artifact_publication_operation(acceptance.operation_id())
            .unwrap();
        assert_eq!(snapshot.state(), ServiceOperationStatus::Failed);
        assert_eq!(snapshot.phase(), ArtifactPublicationPhase::Failed);
        assert_eq!(snapshot.error_code(), Some("publication_secret_detected"));
        assert_eq!(
            *events.lock().unwrap(),
            [
                "scan_artifact",
                "scan_payload",
                "scan_artifact",
                "scan_payload"
            ]
        );
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn startup_recovery_fails_an_unclaimed_publication_without_replay() {
        let fixture = artifact_publication_fixture_with_persistent_ledger(true);
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "publication-startup-recovery");
        let acceptance = service.publish_artifact(&request).unwrap();
        assert_eq!(*events.lock().unwrap(), ["scan_artifact", "scan_payload"]);
        let recovered = service.recover_incomplete_operations().unwrap();
        assert_eq!(recovered, [acceptance.operation_id().clone()]);
        let snapshot = service
            .run_artifact_publication(&acceptance, &request)
            .unwrap();
        assert_eq!(snapshot.state(), ServiceOperationStatus::Failed);
        assert_eq!(snapshot.publication_id(), acceptance.publication_id());
        assert_eq!(snapshot.phase(), ArtifactPublicationPhase::Failed);
        assert_eq!(snapshot.error_code(), Some("interrupted_before_start"));
        assert_eq!(events.lock().unwrap().len(), 2);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn startup_recovery_marks_a_claimed_publication_as_recovery_required() {
        let fixture = artifact_publication_fixture_with_persistent_ledger(true);
        let lock_probe =
            crate::LedgerRunLock::acquire(fixture.ledger.ledger_path().unwrap()).unwrap();
        drop(lock_probe);
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "publication-running-recovery");
        let acceptance = service.publish_artifact(&request).unwrap();
        {
            let connection = fixture.ledger.lock_connection().unwrap();
            connection
                .execute(
                    "UPDATE service_artifact_publication_operations
                     SET status='running',phase='committing'
                     WHERE id=?1",
                    params![acceptance.operation_id().as_str()],
                )
                .unwrap();
        }

        let recovered = service.recover_incomplete_operations().unwrap();
        assert_eq!(recovered, [acceptance.operation_id().clone()]);
        let snapshot = service
            .get_artifact_publication_operation(acceptance.operation_id())
            .unwrap();
        assert_eq!(snapshot.publication_id(), acceptance.publication_id());
        assert_eq!(snapshot.state(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(snapshot.phase(), ArtifactPublicationPhase::RecoveryRequired);
        assert_eq!(snapshot.error_code(), Some("interrupted"));
        assert_eq!(*events.lock().unwrap(), ["scan_artifact", "scan_payload"]);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn publication_runner_rejects_a_revision_change_after_acceptance() {
        let fixture = artifact_publication_fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "publication-stale-after-acceptance");
        let acceptance = service.publish_artifact(&request).unwrap();
        fixture
            .ledger
            .lock_connection()
            .unwrap()
            .execute(
                "UPDATE service_task_revisions SET revision=revision+1 WHERE task_id=?1",
                params![fixture.task_id.as_str()],
            )
            .unwrap();
        assert!(matches!(
            service.run_artifact_publication(&acceptance, &request),
            Err(ServiceError::StaleRevision { .. })
        ));
        let snapshot = service
            .get_artifact_publication_operation(acceptance.operation_id())
            .unwrap();
        assert_eq!(snapshot.state(), ServiceOperationStatus::Failed);
        assert_eq!(snapshot.error_code(), Some("publication_stale_revision"));
        assert_eq!(*events.lock().unwrap(), ["scan_artifact", "scan_payload"]);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn publication_scan_findings_and_ambiguous_push_fail_closed_without_replay() {
        let fixture = artifact_publication_fixture();
        let events = Arc::new(Mutex::new(Vec::new()));
        let scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Findings),
            events: events.clone(),
        };
        let gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: false,
            fail_pull_request: false,
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&scanner)
        .with_artifact_publication_gateway(&gateway);
        let request = publication_request(&fixture, "secret-denied");
        assert!(matches!(
            service.publish_artifact(&request),
            Err(ServiceError::PolicyDenied(
                "secret scan found sensitive content"
            ))
        ));
        assert_eq!(*events.lock().unwrap(), ["scan_artifact", "scan_payload"]);
        let count: i64 = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM service_artifact_publication_operations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        let clean_scanner = FakeSecretScanner {
            artifact_result: Ok(SecretScanResult::Clean),
            payload_result: Ok(SecretScanResult::Clean),
            events: events.clone(),
        };
        let uncertain_gateway = FakeArtifactPublicationGateway {
            repository: fixture.repo.0.clone(),
            events: events.clone(),
            fail_push: true,
            fail_pull_request: false,
        };
        let recovery_service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_secret_scanner(&clean_scanner)
        .with_artifact_publication_gateway(&uncertain_gateway);
        let request = publication_request(&fixture, "push-may-have-completed");
        let acceptance = recovery_service.publish_artifact(&request).unwrap();
        assert!(matches!(
            recovery_service.run_artifact_publication(&acceptance, &request),
            Err(ServiceError::PublicationRecoveryRequired)
        ));
        let operation_id: OperationId = {
            let connection = fixture.ledger.lock_connection().unwrap();
            OperationId::new(connection.query_row(
                "SELECT id FROM service_artifact_publication_operations WHERE request_id=?1",
                params![request.request_id],
                |row| row.get::<_, String>(0),
            ).unwrap())
        };
        let snapshot = recovery_service
            .get_artifact_publication_operation(&operation_id)
            .unwrap();
        assert_eq!(snapshot.state(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(snapshot.phase(), ArtifactPublicationPhase::RecoveryRequired);
        assert_eq!(snapshot.error_code(), Some("publication_push_ambiguous"));
        assert!(snapshot.commit_sha().is_some());
        assert!(snapshot.pull_request().is_none());
        let events_before_retry = events.lock().unwrap().len();
        let retry = recovery_service.publish_artifact(&request).unwrap();
        assert_eq!(retry.operation_id(), &operation_id);
        assert_eq!(retry.publication_id(), acceptance.publication_id());
        assert_eq!(retry.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(events.lock().unwrap().len(), events_before_retry);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
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

    #[test]
    fn ci_get_by_publication_id_binds_task_pr_and_saved_head_sha() {
        let fixture = artifact_publication_fixture();
        let publication = publish_fixture(&fixture, "ci-publication-get");
        let head_sha = publication.commit_sha().unwrap().to_owned();
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(17), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();

        let observation = service
            .get_ci(&CiServiceTarget::Publication(
                publication.publication_id().clone(),
            ))
            .unwrap();

        assert_eq!(observation.task_id(), Some(&fixture.task_id));
        assert_eq!(observation.target().repository(), "owner/repo");
        assert_eq!(observation.target().pull_request_number(), Some(17));
        assert_eq!(observation.target().head_sha(), head_sha);
        assert_eq!(observation.state(), CiAggregateState::Passed);
        assert_eq!(*provider.hosts.lock().unwrap(), ["github.com"]);
        assert!(matches!(
            provider.queries.lock().unwrap().first(),
            Some(CiQueryTarget::PullRequest {
                repository,
                number: 17,
                expected_head_sha: Some(sha),
            }) if repository == "owner/repo" && sha == &head_sha
        ));
        let persisted = fixture
            .ledger
            .get_ci_observation(observation.id())
            .unwrap()
            .unwrap();
        assert_eq!(persisted, observation);
        let other_task_id = TaskId::new("different-ci-task");
        fixture
            .ledger
            .save_task(&Task::new(
                other_task_id.clone(),
                "different task",
                TaskRole::new("reviewer"),
            ))
            .unwrap();
        let query_count = provider.queries.lock().unwrap().len();
        assert!(matches!(
            service.wait_ci(
                &other_task_id,
                0,
                &CiServiceTarget::Publication(publication.publication_id().clone()),
                Instant::now() + Duration::from_secs(1),
            ),
            Err(ServiceError::PolicyDenied(_))
        ));
        assert_eq!(provider.queries.lock().unwrap().len(), query_count);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_get_does_not_persist_when_publication_head_sha_changed() {
        let fixture = artifact_publication_fixture();
        let publication = publish_fixture(&fixture, "ci-publication-head-mismatch");
        let saved_head_sha = publication.commit_sha().unwrap().to_owned();
        let current_head_sha = if saved_head_sha == "a".repeat(40) {
            "b".repeat(40)
        } else {
            "a".repeat(40)
        };
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(17), &current_head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();

        assert!(matches!(
            service.get_ci(&CiServiceTarget::Publication(
                publication.publication_id().clone(),
            )),
            Err(ServiceError::Ci(CiError::HeadShaMismatch { expected, actual }))
                if expected == saved_head_sha && actual == current_head_sha
        ));
        let count: i64 = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM ci_observations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_publication_url_must_have_an_exact_pr_path_and_number() {
        assert_eq!(
            repository_from_pull_request_url("https://github.com/owner/repo/pull/17", 17)
                .as_deref(),
            Some("owner/repo")
        );
        for invalid in [
            "http://github.com/owner/repo/pull/17",
            "https://github.com/owner/repo/pull/18",
            "https://user@github.com/owner/repo/pull/17",
            "https://github.com/owner/repo/pull/17?query=1",
            "https://github.com/owner/repo/pull/17/files",
            "https://github.com/owner%2Frepo/pull/17",
            "https://github.test/owner/repo/pull/17",
        ] {
            assert_eq!(
                repository_from_pull_request_url(invalid, 17),
                None,
                "{invalid}"
            );
        }
    }

    #[test]
    fn ci_wait_service_attributes_direct_target_to_caller_task_and_persists_observation() {
        let fixture = artifact_publication_fixture();
        let head_sha = "c".repeat(40);
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(4), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let revision = task_revision(&fixture.ledger, &fixture.task_id);

        let observation = service
            .wait_ci(
                &fixture.task_id,
                revision,
                &CiServiceTarget::PullRequest {
                    repository: "owner/repo".into(),
                    number: 4,
                },
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();

        assert_eq!(observation.task_id(), Some(&fixture.task_id));
        assert_eq!(observation.state(), CiAggregateState::Passed);
        assert_eq!(
            fixture
                .ledger
                .get_ci_observation(observation.id())
                .unwrap()
                .unwrap(),
            observation
        );
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn asynchronous_ci_wait_is_idempotently_accepted_and_retrieved() {
        let fixture = artifact_publication_fixture();
        let head_sha = "d".repeat(40);
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(5), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let deadline = SystemTime::now() + Duration::from_secs(2);
        let request = CiWaitRequest::new(
            "ci-wait-idempotency",
            fixture.task_id.clone(),
            task_revision(&fixture.ledger, &fixture.task_id),
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 5,
            },
            deadline,
        );
        let accepted = service.accept_ci_wait("caller-a", &request).unwrap();
        assert_eq!(accepted.status(), ServiceOperationStatus::Accepted);
        assert!(
            provider.queries.lock().unwrap().is_empty(),
            "acceptance must not perform CI I/O"
        );
        assert_eq!(
            service.accept_ci_wait("caller-a", &request).unwrap(),
            accepted
        );
        let other_caller = service.accept_ci_wait("caller-b", &request).unwrap();
        assert_ne!(other_caller.operation_id(), accepted.operation_id());
        let conflict = CiWaitRequest::new(
            "ci-wait-idempotency",
            fixture.task_id.clone(),
            request.expected_revision,
            CiServiceTarget::Commit {
                repository: "owner/repo".into(),
                sha: head_sha,
            },
            deadline,
        );
        assert!(matches!(
            service.accept_ci_wait("caller-a", &conflict),
            Err(ServiceError::IdempotencyConflict)
        ));
        let completed = service
            .run_ci_wait_operation(accepted.operation_id())
            .unwrap();
        assert_eq!(completed.status(), ServiceOperationStatus::Completed);
        assert_eq!(
            completed.observation().unwrap().task_id(),
            Some(&fixture.task_id)
        );
        assert!(matches!(
            service
                .get_operation_result(accepted.operation_id())
                .unwrap(),
            OperationGetResult::CiWait(_)
        ));
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn asynchronous_ci_wait_timeout_exposes_only_details_reference() {
        let fixture = artifact_publication_fixture();
        let head_sha = "e".repeat(40);
        let mut snapshot = ci_provider_snapshot("owner/repo", Some(6), &head_sha);
        snapshot.checks[0].detail_state = CiCheckDetailState::Pending;
        snapshot.checks[0].completed_at = None;
        let provider = FakeCiProvider {
            snapshot,
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let request = CiWaitRequest::new(
            "ci-wait-timeout",
            fixture.task_id.clone(),
            task_revision(&fixture.ledger, &fixture.task_id),
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 6,
            },
            SystemTime::now() + Duration::from_millis(20),
        );
        let accepted = service.accept_ci_wait("caller-a", &request).unwrap();
        let failed = service
            .run_ci_wait_operation(accepted.operation_id())
            .unwrap();
        assert_eq!(failed.status(), ServiceOperationStatus::Failed);
        assert_eq!(failed.error_code(), Some("timeout"));
        assert!(failed.observation().is_none());
        let detail = failed
            .details_ref()
            .expect("last persisted observation reference");
        assert!(fixture.ledger.get_ci_observation(detail).unwrap().is_some());
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_wait_preparation_failure_finishes_claimed_operation() {
        let fixture = artifact_publication_fixture();
        let head_sha = "8".repeat(40);
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(8), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let request = CiWaitRequest::new(
            "ci-wait-bad-stored-target",
            fixture.task_id.clone(),
            task_revision(&fixture.ledger, &fixture.task_id),
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 8,
            },
            SystemTime::now() + Duration::from_secs(5),
        );
        let accepted = service.accept_ci_wait("caller-a", &request).unwrap();
        fixture
            .ledger
            .lock_connection()
            .unwrap()
            .execute(
                "UPDATE service_ci_wait_operations SET target_json='not-json' WHERE id=?1",
                params![accepted.operation_id().as_str()],
            )
            .unwrap();

        let failed = service
            .run_ci_wait_operation(accepted.operation_id())
            .unwrap();
        assert_eq!(failed.status(), ServiceOperationStatus::Failed);
        assert_eq!(failed.error_code(), Some("internal_error"));
        assert!(failed.observation().is_none());
        assert_eq!(
            service
                .run_ci_wait_operation(accepted.operation_id())
                .unwrap(),
            failed
        );
        assert!(provider.queries.lock().unwrap().is_empty());
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_wait_rejects_invalid_new_requests_but_replays_expired_acceptance() {
        let fixture = artifact_publication_fixture();
        let head_sha = "7".repeat(40);
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(8), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let revision = task_revision(&fixture.ledger, &fixture.task_id);
        let future = SystemTime::now() + Duration::from_secs(5);
        let direct = CiServiceTarget::PullRequest {
            repository: "owner/repo".into(),
            number: 8,
        };

        for request in [
            CiWaitRequest::new(
                " ",
                fixture.task_id.clone(),
                revision,
                direct.clone(),
                future,
            ),
            CiWaitRequest::new(
                "ci-wait-past",
                fixture.task_id.clone(),
                revision,
                direct.clone(),
                SystemTime::now() - Duration::from_secs(1),
            ),
            CiWaitRequest::new(
                "ci-wait-bad-repository",
                fixture.task_id.clone(),
                revision,
                CiServiceTarget::PullRequest {
                    repository: "owner/repo/extra".into(),
                    number: 8,
                },
                future,
            ),
            CiWaitRequest::new(
                "ci-wait-bad-sha",
                fixture.task_id.clone(),
                revision,
                CiServiceTarget::Commit {
                    repository: "owner/repo".into(),
                    sha: "short".into(),
                },
                future,
            ),
        ] {
            assert!(matches!(
                service.accept_ci_wait("caller-a", &request),
                Err(ServiceError::InvalidRequest(_))
            ));
        }

        let replay_deadline = SystemTime::now() + Duration::from_millis(150);
        let request = CiWaitRequest::new(
            "ci-wait-expired-replay",
            fixture.task_id.clone(),
            revision,
            direct.clone(),
            replay_deadline,
        );
        let accepted = service.accept_ci_wait("caller-a", &request).unwrap();
        let precise_conflict = CiWaitRequest::new(
            "ci-wait-expired-replay",
            fixture.task_id.clone(),
            revision,
            direct.clone(),
            replay_deadline + Duration::from_nanos(1),
        );
        assert!(matches!(
            service.accept_ci_wait("caller-a", &precise_conflict),
            Err(ServiceError::IdempotencyConflict)
        ));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            service.accept_ci_wait("caller-a", &request).unwrap(),
            accepted
        );
        assert!(provider.queries.lock().unwrap().is_empty());
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_wait_cancel_before_run_is_terminal_without_provider_io() {
        let fixture = artifact_publication_fixture();
        let head_sha = "c".repeat(40);
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(9), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let request = CiWaitRequest::new(
            "ci-wait-cancel-before-run",
            fixture.task_id.clone(),
            task_revision(&fixture.ledger, &fixture.task_id),
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 9,
            },
            SystemTime::now() + Duration::from_secs(10),
        );
        let accepted = service.accept_ci_wait("caller-a", &request).unwrap();
        assert_eq!(
            service
                .request_ci_wait_cancellation(accepted.operation_id())
                .unwrap(),
            CiWaitCancelTargetState::Cancelled
        );
        let snapshot = service
            .run_ci_wait_operation(accepted.operation_id())
            .unwrap();
        assert_eq!(snapshot.status(), ServiceOperationStatus::Cancelled);
        assert_eq!(snapshot.error_code(), Some("cancelled"));
        assert!(provider.queries.lock().unwrap().is_empty());
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_wait_running_cancel_waits_for_worker_stop_confirmation() {
        let fixture = artifact_publication_fixture();
        let resolver = SyncTestResolver;
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let provider = BlockingCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(10), &"a".repeat(40)),
            entered: entered.clone(),
            release: release.clone(),
        };
        let worker_service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &resolver,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(2))
        .unwrap();
        let cancel_service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &resolver,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(2))
        .unwrap();
        let request = CiWaitRequest::new(
            "ci-wait-running-cancel",
            fixture.task_id.clone(),
            task_revision(&fixture.ledger, &fixture.task_id),
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 10,
            },
            SystemTime::now() + Duration::from_secs(10),
        );
        let accepted = worker_service.accept_ci_wait("caller-a", &request).unwrap();
        thread::scope(|scope| {
            let operation_id = accepted.operation_id().clone();
            let worker =
                scope.spawn(move || worker_service.run_ci_wait_operation(&operation_id).unwrap());
            entered.wait();
            let duplicate = cancel_service
                .run_ci_wait_operation(accepted.operation_id())
                .unwrap();
            assert_eq!(duplicate.status(), ServiceOperationStatus::Running);
            assert_eq!(
                cancel_service
                    .request_ci_wait_cancellation(accepted.operation_id())
                    .unwrap(),
                CiWaitCancelTargetState::Running
            );
            assert_eq!(
                cancel_service
                    .get_ci_wait_operation(accepted.operation_id())
                    .unwrap()
                    .status(),
                ServiceOperationStatus::Cancelling
            );
            release.wait();
            let stopped = worker.join().unwrap();
            assert_eq!(stopped.status(), ServiceOperationStatus::Cancelled);
            assert_eq!(stopped.error_code(), Some("cancelled"));
        });
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_wait_cancel_without_registered_worker_requires_recovery() {
        let fixture = artifact_publication_fixture();
        let head_sha = "b".repeat(40);
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(11), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let request = CiWaitRequest::new(
            "ci-wait-lost-worker",
            fixture.task_id.clone(),
            task_revision(&fixture.ledger, &fixture.task_id),
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 11,
            },
            SystemTime::now() + Duration::from_secs(10),
        );
        let accepted = service.accept_ci_wait("caller-a", &request).unwrap();
        fixture
            .ledger
            .lock_connection()
            .unwrap()
            .execute(
                "UPDATE service_ci_wait_operations SET status='running' WHERE id=?1",
                params![accepted.operation_id().as_str()],
            )
            .unwrap();
        assert_eq!(
            service
                .request_ci_wait_cancellation(accepted.operation_id())
                .unwrap(),
            CiWaitCancelTargetState::RecoveryRequired
        );
        let snapshot = service
            .run_ci_wait_operation(accepted.operation_id())
            .unwrap();
        assert_eq!(snapshot.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(snapshot.error_code(), Some("cancel_worker_unavailable"));
        assert!(provider.queries.lock().unwrap().is_empty());
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn startup_recovery_marks_accepted_ci_wait_without_replaying_it() {
        let fixture = artifact_publication_fixture_with_persistent_ledger(true);
        let head_sha = "f".repeat(40);
        let provider = FakeCiProvider {
            snapshot: ci_provider_snapshot("owner/repo", Some(7), &head_sha),
            queries: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
        };
        let request = CiWaitRequest::new(
            "ci-wait-interrupted",
            fixture.task_id.clone(),
            task_revision(&fixture.ledger, &fixture.task_id),
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 7,
            },
            SystemTime::now() + Duration::from_secs(10),
        );
        let accepted = {
            let service = OperationService::new(
                &fixture.ledger,
                &fixture.workspace,
                &fixture.providers,
                3,
                Duration::from_secs(30),
            )
            .unwrap()
            .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
            .unwrap();
            let accepted = service.accept_ci_wait("caller-a", &request).unwrap();
            drop(service);
            accepted
        };
        let database_path = fixture.repo.0.join("service-ledger.sqlite");
        drop(fixture.ledger);
        let ledger = SqliteExecutionLedger::open(database_path).unwrap();
        let service = OperationService::new(
            &ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap()
        .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
        .unwrap();
        let recovered = service
            .run_ci_wait_operation(accepted.operation_id())
            .unwrap();
        assert_eq!(recovered.status(), ServiceOperationStatus::RecoveryRequired);
        assert_eq!(recovered.error_code(), Some("interrupted"));
        assert!(recovered.observation().is_none());
        assert!(provider.queries.lock().unwrap().is_empty());
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }

    #[test]
    fn ci_wait_acceptance_rechecks_revision_after_a_concurrent_task_update() {
        let fixture = artifact_publication_fixture();
        let path = std::env::temp_dir().join(format!(
            "ci-wait-race-{}-{}.sqlite",
            std::process::id(),
            now_ms()
        ));
        let ledger = SqliteExecutionLedger::open(&path).unwrap();
        let task_id = TaskId::new("ci-wait-race-task");
        ledger
            .save_task(&Task::new(
                task_id.clone(),
                "race task",
                TaskRole::new("reviewer"),
            ))
            .unwrap();
        let current_revision = 4_u64;
        {
            let connection = ledger.lock_connection().unwrap();
            connection
                .execute(
                    "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
                    params![task_id.as_str(), current_revision as i64],
                )
                .unwrap();
        }
        let head_sha = "9".repeat(40);
        let queries = Arc::new(Mutex::new(Vec::new()));
        let request = CiWaitRequest::new(
            "ci-wait-race-request",
            task_id.clone(),
            current_revision,
            CiServiceTarget::PullRequest {
                repository: "owner/repo".into(),
                number: 9,
            },
            SystemTime::now() + Duration::from_secs(5),
        );
        let mut competing = rusqlite::Connection::open(&path).unwrap();
        let tx = competing
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![task_id.as_str(), (current_revision + 1) as i64],
        )
        .unwrap();
        std::thread::scope(|scope| {
            let ledger = &ledger;
            let workspace = &fixture.workspace;
            let queries = Arc::clone(&queries);
            let request = request.clone();
            let acceptance = scope.spawn(move || {
                let providers = ProviderRegistry::new();
                let provider = FakeCiProvider {
                    snapshot: ci_provider_snapshot("owner/repo", Some(9), &head_sha),
                    queries: Mutex::new(Vec::new()),
                    hosts: Mutex::new(Vec::new()),
                };
                let service = OperationService::new(
                    ledger,
                    workspace,
                    &providers,
                    3,
                    Duration::from_secs(30),
                )
                .unwrap()
                .with_ci_provider(&provider, Duration::from_millis(1), Duration::from_secs(1))
                .unwrap();
                let result = service.accept_ci_wait("caller-a", &request);
                *queries.lock().unwrap() = provider.queries.lock().unwrap().clone();
                result
            });
            std::thread::sleep(Duration::from_millis(50));
            tx.commit().unwrap();
            assert!(matches!(
                acceptance.join().unwrap(),
                Err(ServiceError::StaleRevision {
                    expected: 4,
                    actual: 5
                })
            ));
        });
        assert!(queries.lock().unwrap().is_empty());
        drop(ledger);
        let _ = std::fs::remove_file(&path);
        cleanup_fixture_worktree(
            &fixture.repo,
            &fixture.workspace,
            &fixture.task_id,
            &fixture.attempt_id,
        );
    }
    #[test]
    fn cancellation_fails_closed_for_validation_targets() {
        let fixture = artifact_publication_fixture();
        let validation_operation_id = "validation-active-no-stop-hook";
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap();
        {
            let connection = fixture.ledger.lock_connection().unwrap();
            connection.execute(
                "INSERT INTO service_validation_operations(id,task_id,request_id,expected_revision,accepted_revision,revision,artifact_id,profile_id,checks_json,status,accepted_at) VALUES(?1,?2,'validation-request',0,0,0,'missing-artifact',NULL,'[]','running',?3)",
                params![validation_operation_id, fixture.task_id.as_str(), now_ms()],
            ).unwrap();
            connection.execute(
                "INSERT INTO service_artifact_publication_operations(id,request_id,task_id,request_digest,artifact_id,tree_oid,base_commit,validation_id,decision_id,base_branch,head_branch,status,phase,accepted_revision,revision,accepted_at) VALUES('publication-active-no-stop-hook','publication-request',?1,'digest','artifact','tree','base','validation','decision','main','agent','running','workspace_prepared',0,0,?2)",
                params![fixture.task_id.as_str(), now_ms()],
            ).unwrap();
        }
        let revision = task_revision(&fixture.ledger, &fixture.task_id);
        assert!(matches!(
            service.accept_cancellation(
                "trusted-caller",
                "cancel-validation",
                &fixture.task_id,
                revision,
                "operation.cancel",
                Some(&OperationId::new(validation_operation_id)),
                None,
            ),
            Err(ServiceError::NotCancellable)
        ));
        assert!(matches!(
            service.accept_cancellation(
                "trusted-caller",
                "cancel-publication",
                &fixture.task_id,
                revision,
                "operation.cancel",
                Some(&OperationId::new("publication-active-no-stop-hook")),
                None,
            ),
            Err(ServiceError::NotCancellable)
        ));
        assert!(matches!(
            service.accept_cancellation(
                "trusted-caller",
                "cancel-task-with-validation",
                &fixture.task_id,
                revision,
                "task.cancel",
                None,
                None,
            ),
            Err(ServiceError::NotCancellable)
        ));
        let cancellation_count: i64 = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM service_cancellation_operations WHERE task_id=?1",
                params![fixture.task_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cancellation_count, 0);
    }

    #[test]
    fn previously_accepted_unsupported_cancellation_fails_instead_of_claiming_success() {
        let fixture = artifact_publication_fixture();
        let target_id = "validation-running-before-restart";
        let cancellation_id = OperationId::new("cancel-accepted-before-target-race");
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap();
        let revision = task_revision(&fixture.ledger, &fixture.task_id);
        {
            let connection = fixture.ledger.lock_connection().unwrap();
            connection.execute(
                "INSERT INTO service_validation_operations(id,task_id,request_id,expected_revision,accepted_revision,revision,artifact_id,profile_id,checks_json,status,accepted_at) VALUES(?1,?2,'validation-request',?3,?3,?3,'missing-artifact',NULL,'[]','running',?4)",
                params![target_id, fixture.task_id.as_str(), revision as i64, now_ms()],
            ).unwrap();
            connection.execute(
                "INSERT INTO service_cancellation_operations(id,task_id,kind,target_operation_id,target_ids_json,status,accepted_revision,revision,accepted_at) VALUES(?1,?2,'task.cancel',NULL,?3,'accepted',?4,?4,?5)",
                params![cancellation_id.as_str(), fixture.task_id.as_str(), serde_json::json!([{"kind":"validation.run","id":target_id}]).to_string(), revision as i64, now_ms()],
            ).unwrap();
        }
        let result = service
            .run_cancellation_operation(&cancellation_id)
            .unwrap();
        assert_eq!(result.status(), ServiceOperationStatus::Failed);
        assert_eq!(result.error_code(), Some("not_cancellable"));
        let task_state: String = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT state FROM tasks WHERE id=?1",
                params![fixture.task_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(task_state, "active");
    }
    #[test]
    fn restart_never_completes_cancellation_of_unsupported_targets() {
        let fixture = artifact_publication_fixture_with_persistent_ledger(true);
        let target_id = "validation-running-at-crash";
        let cancellation_id = OperationId::new("cancel-accepted-at-crash");
        let revision = task_revision(&fixture.ledger, &fixture.task_id);
        {
            let connection = fixture.ledger.lock_connection().unwrap();
            connection.execute(
                "INSERT INTO service_validation_operations(id,task_id,request_id,expected_revision,accepted_revision,revision,artifact_id,profile_id,checks_json,status,accepted_at) VALUES(?1,?2,'validation-request',?3,?3,?3,'missing-artifact',NULL,'[]','running',?4)",
                params![target_id, fixture.task_id.as_str(), revision as i64, now_ms()],
            ).unwrap();
            connection.execute(
                "INSERT INTO service_cancellation_operations(id,task_id,kind,target_operation_id,target_ids_json,status,accepted_revision,revision,accepted_at) VALUES(?1,?2,'task.cancel',NULL,?3,'accepted',?4,?4,?5)",
                params![cancellation_id.as_str(), fixture.task_id.as_str(), serde_json::json!([{"kind":"validation.run","id":target_id}]).to_string(), revision as i64, now_ms()],
            ).unwrap();
        }
        let service = OperationService::new(
            &fixture.ledger,
            &fixture.workspace,
            &fixture.providers,
            3,
            Duration::from_secs(30),
        )
        .unwrap();
        let result = service
            .get_cancellation_operation(&cancellation_id)
            .unwrap();
        assert_eq!(result.status(), ServiceOperationStatus::RecoveryRequired);
        let task_state: String = fixture
            .ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT state FROM tasks WHERE id=?1",
                params![fixture.task_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(task_state, "active");
    }
}
