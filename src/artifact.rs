//! Git-tree Artifacts persisted in the same SQLite file as Tasks and Attempts.

use std::{
    ffi::OsString,
    fmt, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{
    AttemptId, LedgerError, ProcessError, ProcessRequest, ProcessRunner, SqliteExecutionLedger,
    TaskId, ValidationResult, Workspace, WorkspaceError, WorkspaceManager,
};

const REF_PREFIX: &str = "refs/ai-dev-orchestrator/artifacts/";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRecord {
    id: String,
    task_id: TaskId,
    source_attempt_id: Option<String>,
    input_artifact_id: Option<String>,
    base_commit: String,
    tree_oid: String,
    repository_root: PathBuf,
    ref_name: String,
    state: ArtifactState,
}

impl ArtifactRecord {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn source_attempt_id(&self) -> Option<&str> {
        self.source_attempt_id.as_deref()
    }
    pub fn input_artifact_id(&self) -> Option<&str> {
        self.input_artifact_id.as_deref()
    }
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }
    pub fn tree_oid(&self) -> &str {
        &self.tree_oid
    }
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }
    pub fn state(&self) -> ArtifactState {
        self.state
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactState {
    PendingRef,
    Available,
    RecoveryRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexDecisionKind {
    Accepted,
    Rejected,
    ChangesRequested,
}

impl CodexDecisionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::ChangesRequested => "changes_requested",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactValidationRecord {
    id: String,
    task_id: TaskId,
    artifact_id: String,
    tree_oid: String,
    passed: bool,
    check_count: usize,
    revision: u64,
}

impl ArtifactValidationRecord {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
    pub fn tree_oid(&self) -> &str {
        &self.tree_oid
    }
    pub const fn passed(&self) -> bool {
        self.passed
    }
    pub const fn check_count(&self) -> usize {
        self.check_count
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactCodexDecisionRecord {
    id: String,
    task_id: TaskId,
    artifact_id: String,
    tree_oid: String,
    decision: CodexDecisionKind,
    reason: String,
    evidence: Vec<(String, String)>,
    revision: u64,
}

impl ArtifactCodexDecisionRecord {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
    pub fn tree_oid(&self) -> &str {
        &self.tree_oid
    }
    pub const fn decision(&self) -> CodexDecisionKind {
        self.decision
    }
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn evidence(&self) -> &[(String, String)] {
        &self.evidence
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactPublicationPermit {
    artifact_id: String,
    tree_oid: String,
    base_commit: String,
    validation_id: String,
    decision_id: String,
}

impl ArtifactPublicationPermit {
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }
    pub fn tree_oid(&self) -> &str {
        &self.tree_oid
    }
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }
    pub fn validation_id(&self) -> &str {
        &self.validation_id
    }
    pub fn decision_id(&self) -> &str {
        &self.decision_id
    }
}

#[derive(Debug)]
pub enum ArtifactError {
    Ledger(LedgerError),
    Workspace(WorkspaceError),
    Git(String),
    Io(String),
    NotFound,
    TaskNotFound,
    StaleRevision {
        expected: u64,
        actual: u64,
    },
    Invalid(String),
    RecoveryRequired,
    WorkspaceChanged {
        expected: String,
        actual: String,
    },
    WorkspaceRetained {
        path: std::path::PathBuf,
        reason: String,
    },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ledger(e) => write!(f, "artifact ledger error: {e}"),
            Self::Workspace(e) => write!(f, "artifact workspace error: {e}"),
            Self::Git(e) => write!(f, "artifact Git error: {e}"),
            Self::Io(e) => write!(f, "artifact I/O error: {e}"),
            Self::NotFound => f.write_str("artifact was not found"),
            Self::TaskNotFound => f.write_str("Task was not found"),
            Self::StaleRevision { expected, actual } => {
                write!(
                    f,
                    "stale Task revision: expected {expected}, actual {actual}"
                )
            }
            Self::Invalid(e) => write!(f, "invalid artifact: {e}"),
            Self::RecoveryRequired => f.write_str("artifact requires recovery"),
            Self::WorkspaceChanged { expected, actual } => {
                write!(f, "workspace changed: expected {expected}, found {actual}")
            }
            Self::WorkspaceRetained { path, reason } => write!(
                f,
                "validation workspace retained at '{}': {reason}",
                path.display()
            ),
        }
    }
}
impl std::error::Error for ArtifactError {}
impl From<LedgerError> for ArtifactError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}
impl From<rusqlite::Error> for ArtifactError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Ledger(LedgerError::from(e))
    }
}
impl From<WorkspaceError> for ArtifactError {
    fn from(e: WorkspaceError) -> Self {
        Self::Workspace(e)
    }
}

pub(crate) struct ArtifactManager<'a> {
    workspaces: &'a WorkspaceManager,
    ledger: &'a SqliteExecutionLedger,
    runner: ProcessRunner,
    git_executable: OsString,
}

impl<'a> ArtifactManager<'a> {
    pub(crate) fn new(workspaces: &'a WorkspaceManager, ledger: &'a SqliteExecutionLedger) -> Self {
        Self {
            workspaces,
            ledger,
            runner: ProcessRunner,
            git_executable: OsString::from("git"),
        }
    }

    pub(crate) fn read(&self, task: &TaskId, id: &str) -> Result<ArtifactRecord, ArtifactError> {
        let mut record = self.load(task, id)?.ok_or(ArtifactError::NotFound)?;
        if record.repository_root != self.workspaces.repository_root() {
            return Err(ArtifactError::Invalid("repository mismatch".into()));
        }
        if record.ref_name != format!("{REF_PREFIX}{}", record.id)
            || !record
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(ArtifactError::Invalid(
                "invalid dedicated Artifact ref".into(),
            ));
        }
        if record.state == ArtifactState::RecoveryRequired {
            return Err(ArtifactError::RecoveryRequired);
        }
        if let Err(error) = self.ensure_tree(&record) {
            if matches!(error, ArtifactError::Invalid(_)) {
                self.set_state(task, id, ArtifactState::RecoveryRequired)?;
                return Err(ArtifactError::RecoveryRequired);
            }
            return Err(error);
        }
        match self.ref_target(&record)? {
            Some(target) if target == record.tree_oid => {}
            Some(_) => {
                self.set_state(task, id, ArtifactState::RecoveryRequired)?;
                return Err(ArtifactError::RecoveryRequired);
            }
            None => self.install_ref(&record)?,
        }
        if record.state == ArtifactState::PendingRef {
            self.set_state(task, id, ArtifactState::Available)?;
            record.state = ArtifactState::Available;
        }
        Ok(record)
    }

    pub(crate) fn materialize(
        &self,
        workspace: &Workspace,
        task: &TaskId,
        attempt: &AttemptId,
        id: &str,
    ) -> Result<(), ArtifactError> {
        let record = self.read(task, id)?;
        if record.repository_root != workspace.repository_root() {
            return Err(ArtifactError::Invalid("repository mismatch".into()));
        }
        self.workspaces.ensure_fresh_artifact_workspace(
            workspace,
            task,
            attempt,
            &record.base_commit,
        )?;
        self.run_git(
            workspace.path(),
            &["read-tree", "--reset", "-u", &record.tree_oid],
            &[],
        )?;
        let actual = self.snapshot(workspace.path())?;
        if actual != record.tree_oid {
            return Err(ArtifactError::WorkspaceChanged {
                expected: record.tree_oid,
                actual,
            });
        }
        Ok(())
    }

    pub(crate) fn capture(
        &self,
        workspace: &Workspace,
        task: &TaskId,
        attempt: &AttemptId,
        input_id: Option<&str>,
        initial_base: &str,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.workspaces
            .validate_artifact_workspace(workspace, task, attempt)?;
        let base_commit = if let Some(id) = input_id {
            self.read(task, id)?.base_commit
        } else {
            initial_base.to_owned()
        };
        let base_commit = self.normalize_commit(workspace.path(), &base_commit)?;
        let tree_oid = self.snapshot(workspace.path())?;
        let id = new_id();
        let ref_name = format!("{REF_PREFIX}{id}");
        let root = self.workspaces.repository_root();
        let record = ArtifactRecord {
            id: id.clone(),
            task_id: task.clone(),
            source_attempt_id: Some(attempt.as_str().to_owned()),
            input_artifact_id: input_id.map(str::to_owned),
            base_commit,
            tree_oid,
            repository_root: root.to_owned(),
            ref_name,
            state: ArtifactState::PendingRef,
        };
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(parent) = input_id {
            let available: Option<String> = tx.query_row("SELECT id FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'", params![task.as_str(), parent], |row| row.get(0)).optional()?;
            if available.is_none() {
                return Err(ArtifactError::NotFound);
            }
        }
        tx.execute("INSERT INTO service_artifacts(id,task_id,source_attempt_id,input_artifact_id,base_commit,tree_oid,repository_root,ref_name,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending_ref',?9)", params![record.id, task.as_str(), attempt.as_str(), input_id, record.base_commit, record.tree_oid, root.to_string_lossy().as_ref(), record.ref_name, now_ms()])?;
        tx.execute("UPDATE service_attempt_artifacts SET output_artifact_id=?3 WHERE task_id=?1 AND attempt_id=?2", params![task.as_str(), attempt.as_str(), record.id])?;
        tx.commit()?;
        drop(connection);
        self.install_ref(&record)?;
        self.set_state(task, &record.id, ArtifactState::Available)?;
        let mut available = record;
        available.state = ArtifactState::Available;
        Ok(available)
    }

    pub(crate) fn verify_input(
        &self,
        task: &TaskId,
        id: &str,
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.read(task, id)
    }

    pub(crate) fn check_revision(
        &self,
        task: &TaskId,
        expected_revision: u64,
    ) -> Result<(), ArtifactError> {
        let connection = self.ledger.lock_connection()?;
        let revision: Option<i64> = connection
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if revision.and_then(|value| u64::try_from(value).ok()) != Some(expected_revision) {
            return Err(match revision.and_then(|value| u64::try_from(value).ok()) {
                Some(actual) => ArtifactError::StaleRevision {
                    expected: expected_revision,
                    actual,
                },
                None => ArtifactError::TaskNotFound,
            });
        }
        Ok(())
    }

    pub(crate) fn record_validation(
        &self,
        task: &TaskId,
        artifact_id: &str,
        expected_revision: u64,
        result: ValidationResult,
    ) -> Result<ArtifactValidationRecord, ArtifactError> {
        if result.passed() && result.checks().is_empty() {
            return Err(ArtifactError::Invalid(
                "a passed validation must contain at least one check".into(),
            ));
        }
        let artifact = self.read(task, artifact_id)?;
        let id = format!("validation-{}", new_id());
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let revision = revision.ok_or(ArtifactError::TaskNotFound)?;
        if u64::try_from(revision).ok() != Some(expected_revision) {
            return Err(ArtifactError::StaleRevision {
                expected: expected_revision,
                actual: u64::try_from(revision).unwrap_or_default(),
            });
        }
        Self::ensure_no_active_publication(&tx, task)?;
        let still_available: Option<String> = tx.query_row(
            "SELECT tree_oid FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'",
            params![task.as_str(), artifact_id],
            |row| row.get(0),
        ).optional()?;
        if still_available.as_deref() != Some(artifact.tree_oid()) {
            return Err(ArtifactError::RecoveryRequired);
        }
        tx.execute(
            "INSERT INTO artifact_validations(id,task_id,artifact_id,tree_oid,summary,passed,created_at) VALUES(?1,?2,?3,?4,'validation outcome recorded; raw diagnostics withheld',?5,?6)",
            params![id, task.as_str(), artifact_id, artifact.tree_oid(), i64::from(result.passed()), now_ms()],
        )?;
        for (sequence, check) in result.checks().iter().enumerate() {
            tx.execute(
                "INSERT INTO artifact_validation_checks(validation_id,sequence,name,passed,exit_status,diagnostics) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id, i64::try_from(sequence).map_err(|_| ArtifactError::Invalid("too many validation checks".into()))?, format!("check-{}", sequence + 1), i64::from(check.passed()), check.exit_status(), "raw diagnostics withheld until redaction is configured"],
            )?;
        }
        let next_revision = expected_revision
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| ArtifactError::Invalid("Task revision overflow".into()))?;
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![task.as_str(), next_revision as i64],
        )?;
        tx.commit()?;
        Ok(ArtifactValidationRecord {
            id,
            task_id: task.clone(),
            artifact_id: artifact_id.to_owned(),
            tree_oid: artifact.tree_oid,
            passed: result.passed(),
            check_count: result.checks().len(),
            revision: next_revision,
        })
    }

    pub(crate) fn record_decision(
        &self,
        task: &TaskId,
        artifact_id: &str,
        expected_revision: u64,
        decision: CodexDecisionKind,
        reason: &str,
        evidence: &[(String, String)],
    ) -> Result<ArtifactCodexDecisionRecord, ArtifactError> {
        if reason.trim().is_empty() {
            return Err(ArtifactError::Invalid(
                "decision reason must not be empty".into(),
            ));
        }
        self.check_revision(task, expected_revision)?;
        let artifact = self.read(task, artifact_id)?;
        let id = format!("decision-{}", new_id());
        let evidence_json = serde_json::to_string(evidence)
            .map_err(|error| ArtifactError::Invalid(error.to_string()))?;
        let mut connection = self.ledger.lock_connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let revision = revision.ok_or(ArtifactError::TaskNotFound)?;
        if u64::try_from(revision).ok() != Some(expected_revision) {
            return Err(ArtifactError::StaleRevision {
                expected: expected_revision,
                actual: u64::try_from(revision).unwrap_or_default(),
            });
        }
        Self::ensure_no_active_publication(&tx, task)?;
        let still_available: Option<String> = tx.query_row(
            "SELECT tree_oid FROM service_artifacts WHERE task_id=?1 AND id=?2 AND state='available'",
            params![task.as_str(), artifact_id],
            |row| row.get(0),
        ).optional()?;
        if still_available.as_deref() != Some(artifact.tree_oid()) {
            return Err(ArtifactError::RecoveryRequired);
        }
        for (kind, evidence_id) in evidence {
            if kind != "validation" {
                return Err(ArtifactError::Invalid(
                    "this MVP only accepts stored Validation evidence".into(),
                ));
            }
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM artifact_validations WHERE id=?1 AND task_id=?2 AND artifact_id=?3)",
                params![evidence_id, task.as_str(), artifact_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(ArtifactError::Invalid(
                    "evidence belongs to another Artifact or is missing".into(),
                ));
            }
        }
        let next_revision = expected_revision
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| ArtifactError::Invalid("Task revision overflow".into()))?;
        tx.execute(
            "INSERT INTO artifact_codex_decisions(id,task_id,artifact_id,tree_oid,decision,reason,evidence_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, task.as_str(), artifact_id, artifact.tree_oid(), decision.as_str(), reason, evidence_json, now_ms()],
        )?;
        tx.execute(
            "UPDATE service_task_revisions SET revision=?2 WHERE task_id=?1",
            params![task.as_str(), next_revision as i64],
        )?;
        tx.commit()?;
        Ok(ArtifactCodexDecisionRecord {
            id,
            task_id: task.clone(),
            artifact_id: artifact_id.to_owned(),
            tree_oid: artifact.tree_oid,
            decision,
            reason: reason.to_owned(),
            evidence: evidence.to_vec(),
            revision: next_revision,
        })
    }

    pub(crate) fn publication_permit(
        &self,
        task: &TaskId,
        artifact_id: &str,
        validation_id: &str,
        decision_id: &str,
        expected_revision: u64,
    ) -> Result<ArtifactPublicationPermit, ArtifactError> {
        self.check_revision(task, expected_revision)?;
        let artifact = self.read(task, artifact_id)?;
        let connection = self.ledger.lock_connection()?;
        let revision: Option<i64> = connection
            .query_row(
                "SELECT revision FROM service_task_revisions WHERE task_id=?1",
                params![task.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if revision.and_then(|value| u64::try_from(value).ok()) != Some(expected_revision) {
            return Err(match revision.and_then(|value| u64::try_from(value).ok()) {
                Some(actual) => ArtifactError::StaleRevision {
                    expected: expected_revision,
                    actual,
                },
                None => ArtifactError::TaskNotFound,
            });
        }
        let validation_matches: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifact_validations WHERE id=?1 AND task_id=?2 AND artifact_id=?3 AND tree_oid=?4 AND passed=1)",
            params![validation_id, task.as_str(), artifact_id, artifact.tree_oid()],
            |row| row.get(0),
        )?;
        let decision_evidence: Option<String> = connection.query_row(
            "SELECT evidence_json FROM artifact_codex_decisions WHERE id=?1 AND task_id=?2 AND artifact_id=?3 AND tree_oid=?4 AND decision='accepted' AND rowid=(SELECT MAX(rowid) FROM artifact_codex_decisions WHERE task_id=?2 AND artifact_id=?3)",
            params![decision_id, task.as_str(), artifact_id, artifact.tree_oid()],
            |row| row.get(0),
        ).optional()?;
        let decision_references_validation = decision_evidence
            .map(|json| serde_json::from_str::<Vec<(String, String)>>(&json))
            .transpose()
            .map_err(|error| ArtifactError::Invalid(error.to_string()))?
            .is_some_and(|evidence| {
                evidence
                    .iter()
                    .any(|(kind, id)| kind == "validation" && id == validation_id)
            });
        if !validation_matches || !decision_references_validation {
            return Err(ArtifactError::Invalid(
                "publication evidence is missing, unsuccessful, or belongs to another Artifact"
                    .into(),
            ));
        }
        Ok(ArtifactPublicationPermit {
            artifact_id: artifact.id,
            tree_oid: artifact.tree_oid,
            base_commit: artifact.base_commit,
            validation_id: validation_id.to_owned(),
            decision_id: decision_id.to_owned(),
        })
    }

    fn ensure_no_active_publication(
        tx: &rusqlite::Transaction<'_>,
        task: &TaskId,
    ) -> Result<(), ArtifactError> {
        let active: Option<String> = tx.query_row(
            "SELECT id FROM service_artifact_publication_operations WHERE task_id=?1 AND status IN ('accepted','running','recovery_required') ORDER BY rowid LIMIT 1",
            params![task.as_str()],
            |row| row.get(0),
        ).optional()?;
        if active.is_some() {
            return Err(ArtifactError::Invalid(
                "Task has an active publication operation".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn cleanup_validation_workspace(
        &self,
        task: &TaskId,
        attempt: &AttemptId,
        workspace: &Workspace,
        expected_tree: &str,
    ) -> Result<(), ArtifactError> {
        if !is_validation_attempt(attempt) {
            return Err(ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: "worktree is not identified as a validation workspace".into(),
            });
        }
        self.workspaces
            .validate_artifact_workspace(workspace, task, attempt)
            .map_err(|error| ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            })?;
        if let Err(error) = self.verify_workspace_tree(workspace.path(), expected_tree) {
            return Err(ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            });
        }
        self.workspaces
            .cleanup(workspace)
            .map_err(|error| ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            })?;
        Ok(())
    }

    /// Force-removes a validation-only worktree only after proving that its complete
    /// Git tree still equals the saved Artifact tree. Validation worktrees are created
    /// privately by the Service; any mismatch or failed ownership check preserves it.
    pub(crate) fn cleanup_unchanged_artifact_validation_workspace(
        &self,
        task: &TaskId,
        attempt: &AttemptId,
        artifact_id: &str,
        workspace: &Workspace,
    ) -> Result<(), ArtifactError> {
        if !is_validation_attempt(attempt) {
            return Err(ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: "worktree is not identified as a validation workspace".into(),
            });
        }
        let artifact =
            self.read(task, artifact_id)
                .map_err(|error| ArtifactError::WorkspaceRetained {
                    path: workspace.path().to_owned(),
                    reason: error.to_string(),
                })?;
        self.workspaces
            .validate_artifact_workspace(workspace, task, attempt)
            .map_err(|error| ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            })?;
        let ignored = self
            .run_git(
                workspace.path(),
                &[
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                ],
                &[],
            )
            .map_err(|error| ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            })?;
        if ignored.output_truncated || !ignored.stdout.is_empty() {
            return Err(ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: if ignored.output_truncated {
                    "ignored-file scan was incomplete".into()
                } else {
                    "ignored files are not part of the saved Artifact tree".into()
                },
            });
        }
        let modes = self
            .run_git(
                &artifact.repository_root,
                &[
                    "ls-tree",
                    "-r",
                    "--format=%(objectmode)",
                    artifact.tree_oid(),
                ],
                &[],
            )
            .map_err(|error| ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            })?;
        if modes.output_truncated
            || String::from_utf8_lossy(&modes.stdout)
                .lines()
                .any(|mode| mode == "160000")
        {
            return Err(ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: if modes.output_truncated {
                    "Artifact tree mode scan was incomplete".into()
                } else {
                    "Artifact contains a submodule whose working contents are not represented by its Git tree".into()
                },
            });
        }
        self.workspaces
            .validate_artifact_workspace(workspace, task, attempt)
            .map_err(|error| ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            })?;
        if let Err(error) = self.verify_workspace_tree(workspace.path(), artifact.tree_oid()) {
            return Err(ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            });
        }
        self.workspaces.cleanup_force(workspace).map_err(|error| {
            ArtifactError::WorkspaceRetained {
                path: workspace.path().to_owned(),
                reason: error.to_string(),
            }
        })?;
        Ok(())
    }

    pub(crate) fn retained_workspace_error(
        workspace: &Workspace,
        reason: impl Into<String>,
    ) -> ArtifactError {
        ArtifactError::WorkspaceRetained {
            path: workspace.path().to_owned(),
            reason: reason.into(),
        }
    }

    pub(crate) fn verify_workspace_tree(
        &self,
        path: &Path,
        expected: &str,
    ) -> Result<(), ArtifactError> {
        let actual = self.snapshot(path)?;
        if actual != expected {
            return Err(ArtifactError::WorkspaceChanged {
                expected: expected.to_owned(),
                actual,
            });
        }
        Ok(())
    }

    pub(crate) fn snapshot_workspace(&self, path: &Path) -> Result<String, ArtifactError> {
        self.snapshot(path)
    }

    fn load(&self, task: &TaskId, id: &str) -> Result<Option<ArtifactRecord>, ArtifactError> {
        let connection = self.ledger.lock_connection()?;
        connection.query_row("SELECT id,source_attempt_id,input_artifact_id,base_commit,tree_oid,repository_root,ref_name,state FROM service_artifacts WHERE task_id=?1 AND id=?2", params![task.as_str(), id], |row| {
            let state: String = row.get(7)?;
            let state = match state.as_str() { "pending_ref" => ArtifactState::PendingRef, "available" => ArtifactState::Available, "recovery_required" => ArtifactState::RecoveryRequired, _ => return Err(rusqlite::Error::InvalidQuery) };
            Ok(ArtifactRecord { id: row.get(0)?, task_id: task.clone(), source_attempt_id: row.get(1)?, input_artifact_id: row.get(2)?, base_commit: row.get(3)?, tree_oid: row.get(4)?, repository_root: PathBuf::from(row.get::<_, String>(5)?), ref_name: row.get(6)?, state })
        }).optional().map_err(Into::into)
    }

    fn set_state(
        &self,
        task: &TaskId,
        id: &str,
        state: ArtifactState,
    ) -> Result<(), ArtifactError> {
        let text = match state {
            ArtifactState::PendingRef => "pending_ref",
            ArtifactState::Available => "available",
            ArtifactState::RecoveryRequired => "recovery_required",
        };
        let connection = self.ledger.lock_connection()?;
        connection.execute(
            "UPDATE service_artifacts SET state=?3 WHERE task_id=?1 AND id=?2",
            params![task.as_str(), id, text],
        )?;
        Ok(())
    }

    fn snapshot(&self, path: &Path) -> Result<String, ArtifactError> {
        let temp = TempIndex::create()?;
        let index = temp.path().join("index").to_string_lossy().into_owned();
        let index_env = [("GIT_INDEX_FILE", index)];
        self.run_git(path, &["read-tree", "HEAD"], &index_env)?;
        // Ignored files are deliberately excluded; the temporary index preserves the user's index.
        self.run_git(path, &["add", "-A", "--", "."], &index_env)?;
        let output = self.run_git(path, &["write-tree"], &index_env)?;
        let tree = String::from_utf8(output.stdout)
            .map_err(|_| ArtifactError::Invalid("tree id is not UTF-8".into()))?
            .trim()
            .to_owned();
        if !is_oid(&tree) {
            return Err(ArtifactError::Invalid("invalid Git tree id".into()));
        }
        Ok(tree)
    }

    fn normalize_commit(&self, cwd: &Path, oid: &str) -> Result<String, ArtifactError> {
        if !is_oid(oid) {
            return Err(ArtifactError::Invalid("invalid base commit".into()));
        }
        let expr = format!("{oid}^{{commit}}");
        let output = self.run_git(
            cwd,
            &["rev-parse", "--verify", "--end-of-options", &expr],
            &[],
        )?;
        let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if actual != oid {
            return Err(ArtifactError::Invalid(
                "base commit is not canonical".into(),
            ));
        }
        Ok(actual)
    }

    fn ensure_tree(&self, record: &ArtifactRecord) -> Result<(), ArtifactError> {
        if !is_oid(&record.tree_oid) {
            return Err(ArtifactError::Invalid("invalid Git tree object ID".into()));
        }
        let output = match self.runner.run(
            ProcessRequest::new(self.git_executable.clone())
                .args(["cat-file", "-t", &record.tree_oid])
                .cwd(&record.repository_root),
        ) {
            Ok(output) => output,
            Err(ProcessError::NonZeroExit(output))
                if is_missing_git_object_diagnostic(&output.stderr) =>
            {
                return Err(ArtifactError::Invalid("tree object missing".into()));
            }
            Err(error) => return Err(ArtifactError::Git(process_error(error))),
        };
        if output.stdout_truncated || output.stderr_truncated {
            return Err(ArtifactError::Git("tree type output truncated".into()));
        }
        if String::from_utf8_lossy(&output.stdout).trim() != "tree" {
            return Err(ArtifactError::Invalid("tree object missing".into()));
        }
        Ok(())
    }

    fn ref_target(&self, record: &ArtifactRecord) -> Result<Option<String>, ArtifactError> {
        let output = self.run_git(
            &record.repository_root,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)",
                &record.ref_name,
            ],
            &[],
        )?;
        if output.output_truncated {
            return Err(ArtifactError::Git("ref output truncated".into()));
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some((name, target)) = line.split_once('\0') {
                if name == record.ref_name {
                    return Ok(Some(target.to_owned()));
                }
            }
        }
        Ok(None)
    }

    fn install_ref(&self, record: &ArtifactRecord) -> Result<(), ArtifactError> {
        self.ensure_tree(record)?;
        let zero = "0".repeat(record.tree_oid.len());
        self.run_git(
            &record.repository_root,
            &["update-ref", &record.ref_name, &record.tree_oid, &zero],
            &[],
        )?;
        Ok(())
    }

    fn run_git(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, String)],
    ) -> Result<crate::ProcessOutput, ArtifactError> {
        let mut request = ProcessRequest::new(self.git_executable.clone())
            .args(args.iter().copied())
            .cwd(cwd);
        for (key, value) in env {
            request = request.env(*key, value.clone());
        }
        self.runner
            .run(request)
            .map_err(|e| ArtifactError::Git(process_error(e)))
    }
}

fn is_validation_attempt(attempt: &AttemptId) -> bool {
    attempt
        .as_str()
        .strip_prefix("validation-")
        .is_some_and(|suffix| !suffix.is_empty())
}

struct TempIndex(PathBuf);
impl TempIndex {
    fn create() -> Result<Self, ArtifactError> {
        for _ in 0..10 {
            let path = std::env::temp_dir()
                .join(format!("ai-dev-orchestrator-artifact-index-{}", new_id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(ArtifactError::Io(e.to_string())),
            }
        }
        Err(ArtifactError::Io(
            "could not allocate temporary index".into(),
        ))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn new_id() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("artifact-{time:x}-{:x}-{seq:x}", std::process::id())
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn is_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn is_missing_git_object_diagnostic(stderr: &[u8]) -> bool {
    let diagnostic = String::from_utf8_lossy(stderr);
    diagnostic.contains("Not a valid object name")
        || diagnostic.contains("could not get object info")
}
fn process_error(error: ProcessError) -> String {
    match error {
        ProcessError::NonZeroExit(output) => format!("git exited with status {:?}", output.status),
        _ => "git command could not be completed".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactError, ArtifactManager, CodexDecisionKind, REF_PREFIX,
        is_missing_git_object_diagnostic, new_id,
    };
    use crate::{
        AttemptId, SqliteExecutionLedger, Task, TaskId, TaskRole, ValidationResult,
        WorkspaceManager,
    };
    use std::{fs, process::Command};

    #[test]
    fn only_explicit_missing_object_diagnostics_confirm_absence() {
        assert!(is_missing_git_object_diagnostic(
            b"fatal: git cat-file: could not get object info"
        ));
        assert!(is_missing_git_object_diagnostic(
            b"fatal: Not a valid object name deadbeef"
        ));
        assert!(!is_missing_git_object_diagnostic(
            b"fatal: cannot open .git/objects: I/O error"
        ));
        assert!(!is_missing_git_object_diagnostic(b""));
    }

    #[test]
    fn validation_and_acceptance_are_bound_to_the_same_artifact_tree() {
        let root = std::env::temp_dir().join(format!("artifact-evidence-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        let repository = root.join("repo");
        fs::create_dir_all(&repository).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&repository)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.invalid"]);
        fs::write(repository.join("tracked.txt"), "content\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "base"]);
        let submodule_commit = git(&["rev-parse", "HEAD"]);
        git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{submodule_commit},vendor"),
        ]);
        git(&["commit", "-qm", "base with submodule gitlink"]);
        let base = git(&["rev-parse", "HEAD"]);
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        let repository = fs::canonicalize(repository).unwrap();
        let task = TaskId::new("artifact-evidence-task");
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger
            .save_task(&Task::new(
                task.clone(),
                "evidence",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let insert_artifact = |id: &str| {
            ledger.lock_connection().unwrap().execute(
                "INSERT INTO service_artifacts(id,task_id,base_commit,tree_oid,repository_root,ref_name,state,created_at) VALUES(?1,?2,?3,?4,?5,?6,'available',0)",
                rusqlite::params![id, task.as_str(), base, tree, repository.to_string_lossy().as_ref(), format!("{REF_PREFIX}{id}")],
            ).unwrap();
        };
        insert_artifact("artifact-a");
        insert_artifact("artifact-b");
        let workspaces = WorkspaceManager::new(&repository).unwrap();
        let manager = ArtifactManager::new(&workspaces, &ledger);
        let validation_attempt = AttemptId::new("validation-submodule-fixture");
        let validation_workspace = workspaces
            .create_at_base(&task, &validation_attempt, &base)
            .unwrap();
        manager
            .materialize(
                &validation_workspace,
                &task,
                &validation_attempt,
                "artifact-a",
            )
            .unwrap();
        assert!(matches!(
            manager.cleanup_unchanged_artifact_validation_workspace(
                &task,
                &validation_attempt,
                "artifact-a",
                &validation_workspace,
            ),
            Err(ArtifactError::WorkspaceRetained { path, reason })
                if path == validation_workspace.path() && reason.contains("submodule")
        ));
        assert!(validation_workspace.path().exists());
        workspaces.cleanup_force(&validation_workspace).unwrap();

        let foreign = root.join("foreign-repo");
        fs::create_dir_all(&foreign).unwrap();
        let foreign_git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&foreign)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        foreign_git(&["init", "-q"]);
        foreign_git(&["config", "user.name", "Test"]);
        foreign_git(&["config", "user.email", "test@example.invalid"]);
        fs::write(foreign.join("foreign.txt"), "foreign\n").unwrap();
        foreign_git(&["add", "foreign.txt"]);
        foreign_git(&["commit", "-qm", "foreign base"]);
        let foreign_workspaces = WorkspaceManager::new(&foreign).unwrap();
        let foreign_attempt = AttemptId::new("validation-foreign-owner");
        let foreign_workspace = foreign_workspaces.create(&task, &foreign_attempt).unwrap();
        assert!(matches!(
            manager.cleanup_unchanged_artifact_validation_workspace(
                &task,
                &foreign_attempt,
                "artifact-a",
                &foreign_workspace,
            ),
            Err(ArtifactError::WorkspaceRetained { path, .. })
                if path == foreign_workspace.path()
        ));
        assert!(foreign_workspace.path().exists());
        foreign_workspaces
            .cleanup_force(&foreign_workspace)
            .unwrap();
        let validation = manager
            .record_validation(
                &task,
                "artifact-a",
                0,
                ValidationResult::from_check("test", "passed", true, Some(0), "secret-ish output"),
            )
            .unwrap();
        assert!(validation.passed());
        assert_eq!(validation.check_count(), 1);
        let diagnostics: String = ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT diagnostics FROM artifact_validation_checks WHERE validation_id=?1",
                rusqlite::params![validation.id()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!diagnostics.contains("secret-ish output"));
        assert!(diagnostics.contains("withheld"));

        let decision = manager
            .record_decision(
                &task,
                "artifact-a",
                1,
                CodexDecisionKind::Accepted,
                "The reviewed change satisfies the task.",
                &[("validation".into(), validation.id().into())],
            )
            .unwrap();
        let permit = manager
            .publication_permit(&task, "artifact-a", validation.id(), decision.id(), 2)
            .unwrap();
        assert_eq!(permit.tree_oid(), tree);
        assert_eq!(permit.artifact_id(), "artifact-a");

        let other_decision = manager
            .record_decision(
                &task,
                "artifact-b",
                2,
                CodexDecisionKind::Accepted,
                "Different Artifact.",
                &[],
            )
            .unwrap();
        assert!(matches!(
            manager.publication_permit(
                &task,
                "artifact-a",
                validation.id(),
                other_decision.id(),
                3
            ),
            Err(ArtifactError::Invalid(_))
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn transient_git_failure_does_not_poison_available_artifact() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("artifact-read-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        let repository = root.join("repo");
        fs::create_dir_all(&repository).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repository)
                .status()
                .unwrap()
                .success()
        );
        let repository = fs::canonicalize(repository).unwrap();
        let failing_git = root.join("git-failure");
        fs::write(
            &failing_git,
            "#!/bin/sh\necho 'fatal: temporary object store I/O failure' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&failing_git, fs::Permissions::from_mode(0o755)).unwrap();

        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = TaskId::new("transient-artifact-task");
        ledger
            .save_task(&Task::new(
                task.clone(),
                "artifact recovery test",
                TaskRole::new("implementer"),
            ))
            .unwrap();
        let id = "artifact-transient";
        let ref_name = format!("{REF_PREFIX}{id}");
        ledger
            .lock_connection()
            .unwrap()
            .execute(
                "INSERT INTO service_artifacts(id,task_id,base_commit,tree_oid,repository_root,ref_name,state,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,'available',0)",
                rusqlite::params![
                    id,
                    task.as_str(),
                    "0".repeat(40),
                    "1".repeat(40),
                    repository.to_string_lossy().as_ref(),
                    ref_name
                ],
            )
            .unwrap();
        let workspaces = WorkspaceManager::new(&repository).unwrap();
        let mut artifacts = ArtifactManager::new(&workspaces, &ledger);
        artifacts.git_executable = failing_git.into_os_string();

        let result = artifacts.verify_input(&task, id);
        assert!(matches!(result, Err(ArtifactError::Git(_))), "{result:?}");
        let state: String = ledger
            .lock_connection()
            .unwrap()
            .query_row(
                "SELECT state FROM service_artifacts WHERE task_id=?1 AND id=?2",
                rusqlite::params![task.as_str(), id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "available");
        let _ = fs::remove_dir_all(root);
    }
}
