use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{ProviderRef, TaskId};

const SCHEMA_VERSION: u32 = 2;
const DEFAULT_LOG_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OperationId(String);

impl OperationId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationStatus {
    Accepted,
    Running,
    Succeeded,
    Failed,
    InvalidOutput,
    TimedOut,
    Cancelled,
    Interrupted,
    RecoveryRequired,
}

impl OperationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::InvalidOutput => "invalid_output",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::RecoveryRequired => "recovery_required",
        }
    }
    fn from_str(value: &str) -> Result<Self, LedgerError> {
        match value {
            "accepted" => Ok(Self::Accepted),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "invalid_output" => Ok(Self::InvalidOutput),
            "timed_out" => Ok(Self::TimedOut),
            "cancelled" => Ok(Self::Cancelled),
            "interrupted" => Ok(Self::Interrupted),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(LedgerError::InvalidValue(value.into())),
        }
    }
    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::InvalidOutput | Self::TimedOut | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRequest {
    request_id: String,
    task_id: TaskId,
    task_revision: u64,
    payload: String,
    instruction: String,
    requested_provider: ProviderRef,
    requested_model: Option<String>,
}

impl OperationRequest {
    pub fn new(
        request_id: impl Into<String>,
        task_id: TaskId,
        task_revision: u64,
        payload: impl Into<String>,
        instruction: impl Into<String>,
        requested_provider: ProviderRef,
        requested_model: Option<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            task_id,
            task_revision,
            payload: payload.into(),
            instruction: instruction.into(),
            requested_provider,
            requested_model,
        }
    }
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub fn task_revision(&self) -> u64 {
        self.task_revision
    }
    pub fn payload(&self) -> &str {
        &self.payload
    }
    pub fn instruction(&self) -> &str {
        &self.instruction
    }
    pub fn requested_provider(&self) -> &ProviderRef {
        &self.requested_provider
    }
    pub fn requested_model(&self) -> Option<&str> {
        self.requested_model.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRecord {
    id: OperationId,
    request: OperationRequest,
    status: OperationStatus,
    accepted_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    observed_provider: Option<ProviderRef>,
    observed_model: Option<String>,
    diagnostic: Option<String>,
    artifact_ref: Option<String>,
}

impl OperationRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> Result<Self, rusqlite::Error> {
        let task_id: String = row.get(2)?;
        let status: String = row.get(6)?;
        Ok(Self {
            id: OperationId::new(row.get::<_, String>(0)?),
            request: OperationRequest::new(
                row.get::<_, String>(1)?,
                TaskId::new(task_id),
                row.get::<_, i64>(3)? as u64,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                ProviderRef::new(row.get::<_, String>(7)?),
                row.get(8)?,
            ),
            status: OperationStatus::from_str(&status)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            accepted_at: row.get(9)?,
            started_at: row.get(10)?,
            finished_at: row.get(11)?,
            observed_provider: row.get::<_, Option<String>>(12)?.map(ProviderRef::new),
            observed_model: row.get(13)?,
            diagnostic: row.get(14)?,
            artifact_ref: row.get(15)?,
        })
    }
    pub fn id(&self) -> &OperationId {
        &self.id
    }
    pub fn request(&self) -> &OperationRequest {
        &self.request
    }
    pub fn status(&self) -> OperationStatus {
        self.status
    }
    pub fn accepted_at(&self) -> i64 {
        self.accepted_at
    }
    pub fn started_at(&self) -> Option<i64> {
        self.started_at
    }
    pub fn finished_at(&self) -> Option<i64> {
        self.finished_at
    }
    pub fn observed_provider(&self) -> Option<&ProviderRef> {
        self.observed_provider.as_ref()
    }
    pub fn observed_model(&self) -> Option<&str> {
        self.observed_model.as_deref()
    }
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
    pub fn artifact_ref(&self) -> Option<&str> {
        self.artifact_ref.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    Accepted,
    Started,
    Finished,
    Observation,
    Failure,
    Recovery,
}
impl EventKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Started => "started",
            Self::Finished => "finished",
            Self::Observation => "observation",
            Self::Failure => "failure",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationEvent {
    pub sequence: i64,
    pub kind: EventKind,
    pub occurred_at: i64,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogReference {
    path: PathBuf,
    byte_count: u64,
    truncated: bool,
}
impl LogReference {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn byte_count(&self) -> u64 {
        self.byte_count
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationRecord {
    pub artifact_ref: String,
    pub passed: bool,
    pub summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageRecord {
    pub input_units: Option<String>,
    pub output_units: Option<String>,
    pub cost: Option<String>,
    pub currency: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewRecord {
    pub artifact_ref: String,
    pub verdict: String,
    pub summary: String,
}

/// An immutable Git tree captured from a managed workspace.
///
/// `tree_oid` is the repository's native Git tree object ID. The dedicated ref
/// keeps that tree reachable for as long as this record is retained.
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
    // Keep the constructor explicit: each value maps to one persisted artifact field.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: impl Into<String>,
        task_id: TaskId,
        source_attempt_id: Option<String>,
        input_artifact_id: Option<String>,
        base_commit: impl Into<String>,
        tree_oid: impl Into<String>,
        repository_root: impl Into<PathBuf>,
        ref_name: impl Into<String>,
        state: ArtifactState,
    ) -> Self {
        Self {
            id: id.into(),
            task_id,
            source_attempt_id,
            input_artifact_id,
            base_commit: base_commit.into(),
            tree_oid: tree_oid.into(),
            repository_root: repository_root.into(),
            ref_name: ref_name.into(),
            state,
        }
    }

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
    pub(crate) fn repository_root(&self) -> &Path {
        &self.repository_root
    }
    pub(crate) fn ref_name(&self) -> &str {
        &self.ref_name
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

impl ArtifactState {
    fn as_str(self) -> &'static str {
        match self {
            Self::PendingRef => "pending_ref",
            Self::Available => "available",
            Self::RecoveryRequired => "recovery_required",
        }
    }
    fn parse(value: &str) -> Result<Self, LedgerError> {
        match value {
            "pending_ref" => Ok(Self::PendingRef),
            "available" => Ok(Self::Available),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(LedgerError::InvalidValue(value.into())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationReference {
    pub repository: String,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub pull_request_url: Option<String>,
    pub ci_sha: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryRecord {
    pub operation: OperationId,
    pub status: OperationStatus,
    pub reason: String,
}

#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    Io(io::Error),
    InvalidValue(String),
    RequestConflict { request_id: String },
    StaleRevision { expected: u64, actual: u64 },
    ActiveOperation(OperationId),
    TerminalConflict(OperationId),
    UnsupportedSchema(u32),
}
impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "ledger database error: {e}"),
            Self::Io(e) => write!(f, "ledger log error: {e}"),
            Self::InvalidValue(v) => write!(f, "invalid ledger value: {v}"),
            Self::RequestConflict { request_id } => {
                write!(f, "request id has a different payload: {request_id}")
            }
            Self::StaleRevision { expected, actual } => write!(
                f,
                "stale task revision: expected {expected}, actual {actual}"
            ),
            Self::ActiveOperation(id) => {
                write!(f, "task already has an active operation: {}", id.as_str())
            }
            Self::TerminalConflict(id) => write!(
                f,
                "terminal operation cannot be overwritten: {}",
                id.as_str()
            ),
            Self::UnsupportedSchema(v) => write!(f, "unsupported ledger schema: {v}"),
        }
    }
}
impl std::error::Error for LedgerError {}
impl From<rusqlite::Error> for LedgerError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}
impl From<io::Error> for LedgerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub trait ExecutionLedger {
    fn accept_operation(&self, request: &OperationRequest) -> Result<OperationRecord, LedgerError>;
    fn start_operation(
        &self,
        id: &OperationId,
        observed_provider: &ProviderRef,
        observed_model: Option<&str>,
    ) -> Result<(), LedgerError>;
    fn get_operation(&self, id: &OperationId) -> Result<Option<OperationRecord>, LedgerError>;
    fn finish_operation(
        &self,
        id: &OperationId,
        status: OperationStatus,
        diagnostic: Option<&str>,
    ) -> Result<(), LedgerError>;
}

pub struct SqliteExecutionLedger {
    connection: Mutex<Connection>,
    log_limit: usize,
}

impl SqliteExecutionLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let sidecar = PathBuf::from(format!("{}.operations.sqlite3", path.as_ref().display()));
        Self::from_connection(Connection::open(sidecar)?)
    }
    pub fn open_in_memory() -> Result<Self, LedgerError> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    fn from_connection(connection: Connection) -> Result<Self, LedgerError> {
        let mut connection = connection;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(LedgerError::UnsupportedSchema(version));
        }
        connection.execute_batch("CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL, task_revision INTEGER NOT NULL, payload TEXT NOT NULL, instruction TEXT NOT NULL, requested_provider TEXT NOT NULL, requested_model TEXT, status TEXT NOT NULL, accepted_at INTEGER NOT NULL, started_at INTEGER, finished_at INTEGER, observed_provider TEXT, observed_model TEXT, diagnostic TEXT, artifact_ref TEXT); CREATE TABLE IF NOT EXISTS task_revisions (task_id TEXT PRIMARY KEY, revision INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS operation_events (operation_id TEXT NOT NULL, sequence INTEGER NOT NULL, kind TEXT NOT NULL, occurred_at INTEGER NOT NULL, detail TEXT NOT NULL, PRIMARY KEY(operation_id, sequence)); CREATE TABLE IF NOT EXISTS log_references (operation_id TEXT NOT NULL, stream TEXT NOT NULL, path TEXT NOT NULL, byte_count INTEGER NOT NULL, truncated INTEGER NOT NULL, PRIMARY KEY(operation_id, stream)); CREATE TABLE IF NOT EXISTS validations (operation_id TEXT PRIMARY KEY, artifact_ref TEXT NOT NULL, passed INTEGER NOT NULL, summary TEXT NOT NULL); CREATE TABLE IF NOT EXISTS reviews (operation_id TEXT PRIMARY KEY, artifact_ref TEXT NOT NULL, verdict TEXT NOT NULL, summary TEXT NOT NULL); CREATE TABLE IF NOT EXISTS usage (operation_id TEXT PRIMARY KEY, input_units TEXT, output_units TEXT, cost TEXT, currency TEXT); CREATE TABLE IF NOT EXISTS budget_reservations (operation_id TEXT PRIMARY KEY, amount TEXT NOT NULL, settled_amount TEXT, state TEXT NOT NULL); CREATE TABLE IF NOT EXISTS publications (operation_id TEXT PRIMARY KEY, repository TEXT NOT NULL, branch TEXT, commit_sha TEXT, pull_request_url TEXT, ci_sha TEXT);")?;
        if version < 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch("CREATE TABLE IF NOT EXISTS artifacts (id TEXT PRIMARY KEY NOT NULL, task_id TEXT NOT NULL, source_attempt_id TEXT, input_artifact_id TEXT, base_commit TEXT NOT NULL, tree_oid TEXT NOT NULL, repository_root TEXT NOT NULL, ref_name TEXT NOT NULL UNIQUE, state TEXT NOT NULL CHECK(state IN ('pending_ref','available','recovery_required')), created_at INTEGER NOT NULL, UNIQUE(task_id, id), FOREIGN KEY(task_id, input_artifact_id) REFERENCES artifacts(task_id, id) ON DELETE RESTRICT);")?;
            transaction.pragma_update(None, "user_version", 2)?;
            transaction.commit()?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
            log_limit: DEFAULT_LOG_LIMIT,
        })
    }
    pub fn set_log_limit(&mut self, bytes: usize) {
        self.log_limit = bytes;
    }
    pub fn set_task_revision(&self, task_id: &TaskId, revision: u64) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute("INSERT INTO task_revisions(task_id, revision) VALUES(?1, ?2) ON CONFLICT(task_id) DO UPDATE SET revision=excluded.revision", params![task_id.as_str(), revision])?;
        Ok(())
    }

    pub(crate) fn prepare_artifact(&self, artifact: &ArtifactRecord) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute(
            "INSERT INTO artifacts (id, task_id, source_attempt_id, input_artifact_id, base_commit, tree_oid, repository_root, ref_name, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![artifact.id, artifact.task_id.as_str(), artifact.source_attempt_id,
                artifact.input_artifact_id, artifact.base_commit, artifact.tree_oid,
                artifact.repository_root.to_string_lossy().as_ref(), artifact.ref_name,
                ArtifactState::PendingRef.as_str(), now()],
        )?;
        Ok(())
    }

    pub(crate) fn set_artifact_state(
        &self,
        task_id: &TaskId,
        artifact_id: &str,
        state: ArtifactState,
    ) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        let changed = connection.execute(
            "UPDATE artifacts SET state=?3 WHERE task_id=?1 AND id=?2",
            params![task_id.as_str(), artifact_id, state.as_str()],
        )?;
        if changed != 1 {
            return Err(LedgerError::InvalidValue(format!(
                "artifact not found: {artifact_id}"
            )));
        }
        Ok(())
    }

    pub(crate) fn get_artifact(
        &self,
        task_id: &TaskId,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRecord>, LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.query_row(
            "SELECT id, task_id, source_attempt_id, input_artifact_id, base_commit, tree_oid, repository_root, ref_name, state FROM artifacts WHERE task_id=?1 AND id=?2",
            params![task_id.as_str(), artifact_id],
            |row| {
                let state: String = row.get(8)?;
                Ok(ArtifactRecord::new(
                    row.get::<_, String>(0)?, TaskId::new(row.get::<_, String>(1)?),
                    row.get(2)?, row.get(3)?, row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?, PathBuf::from(row.get::<_, String>(6)?),
                    row.get::<_, String>(7)?, ArtifactState::parse(&state).map_err(|_| rusqlite::Error::InvalidQuery)?,
                ))
            },
        ).optional().map_err(Into::into)
    }
    pub fn append_event(
        &self,
        id: &OperationId,
        kind: EventKind,
        detail: impl Into<String>,
    ) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        let sequence: i64 = connection.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM operation_events WHERE operation_id=?1",
            params![id.as_str()],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT INTO operation_events VALUES(?1, ?2, ?3, ?4, ?5)",
            params![id.as_str(), sequence, kind.as_str(), now(), detail.into()],
        )?;
        Ok(())
    }
    pub fn events(&self, id: &OperationId) -> Result<Vec<OperationEvent>, LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        let mut statement = connection.prepare("SELECT sequence, kind, occurred_at, detail FROM operation_events WHERE operation_id=?1 ORDER BY sequence")?;
        let rows = statement.query_map(params![id.as_str()], |row| {
            let kind: String = row.get(1)?;
            let kind = match kind.as_str() {
                "accepted" => EventKind::Accepted,
                "started" => EventKind::Started,
                "finished" => EventKind::Finished,
                "observation" => EventKind::Observation,
                "failure" => EventKind::Failure,
                "recovery" => EventKind::Recovery,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(OperationEvent {
                sequence: row.get(0)?,
                kind,
                occurred_at: row.get(2)?,
                detail: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
    pub fn start_operation(
        &self,
        id: &OperationId,
        observed_provider: &ProviderRef,
        observed_model: Option<&str>,
    ) -> Result<(), LedgerError> {
        let mut connection = self.connection.lock().expect("ledger mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let started = now();
        let changed = transaction.execute("UPDATE operations SET status='running', started_at=?2, observed_provider=?3, observed_model=?4 WHERE id=?1 AND status='accepted'", params![id.as_str(), started, observed_provider.as_str(), observed_model])?;
        if changed != 1 {
            return Err(LedgerError::InvalidValue(format!(
                "operation is not accepted: {}",
                id.as_str()
            )));
        }
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM operation_events WHERE operation_id=?1",
            params![id.as_str()],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO operation_events VALUES(?1,?2,'started',?3,?4)",
            params![
                id.as_str(),
                sequence,
                started,
                observed_model.unwrap_or(observed_provider.as_str())
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }
    pub fn save_log(
        &self,
        id: &OperationId,
        stream: &str,
        directory: impl AsRef<Path>,
        content: &[u8],
    ) -> Result<LogReference, LedgerError> {
        let limit = self.log_limit.min(content.len());
        let truncated = limit < content.len();
        let directory = directory.as_ref();
        fs::create_dir_all(directory)?;
        let operation_component = safe_log_component(id.as_str())?;
        let stream_component = safe_log_component(stream)?;
        let canonical_directory = directory.canonicalize()?;
        let path =
            canonical_directory.join(format!("{operation_component}-{stream_component}.log"));
        let canonical_parent = path
            .parent()
            .ok_or_else(|| LedgerError::InvalidValue("log path has no parent".into()))?
            .canonicalize()?;
        if canonical_parent != canonical_directory || !path.starts_with(&canonical_directory) {
            return Err(LedgerError::InvalidValue(
                "log path escapes directory".into(),
            ));
        }
        fs::write(&path, &content[..limit])?;
        let reference = LogReference {
            path,
            byte_count: limit as u64,
            truncated,
        };
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO log_references VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                id.as_str(),
                stream,
                reference.path.to_string_lossy().as_ref(),
                reference.byte_count as i64,
                reference.truncated
            ],
        )?;
        Ok(reference)
    }
    pub fn save_validation(
        &self,
        id: &OperationId,
        validation: &ValidationRecord,
    ) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO validations VALUES(?1, ?2, ?3, ?4)",
            params![
                id.as_str(),
                validation.artifact_ref,
                validation.passed,
                validation.summary
            ],
        )?;
        Ok(())
    }
    pub fn save_review(&self, id: &OperationId, review: &ReviewRecord) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO reviews VALUES(?1, ?2, ?3, ?4)",
            params![
                id.as_str(),
                review.artifact_ref,
                review.verdict,
                review.summary
            ],
        )?;
        Ok(())
    }
    pub fn save_usage(&self, id: &OperationId, usage: &UsageRecord) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO usage VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                id.as_str(),
                usage.input_units,
                usage.output_units,
                usage.cost,
                usage.currency
            ],
        )?;
        Ok(())
    }
    pub fn reserve_budget(&self, id: &OperationId, amount: &str) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute(
            "INSERT OR IGNORE INTO budget_reservations VALUES(?1, ?2, NULL, 'reserved')",
            params![id.as_str(), amount],
        )?;
        Ok(())
    }
    pub fn settle_budget(&self, id: &OperationId, amount: &str) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute("UPDATE budget_reservations SET settled_amount=?2, state='settled' WHERE operation_id=?1 AND state='reserved'", params![id.as_str(), amount])?;
        Ok(())
    }
    pub fn save_publication(
        &self,
        id: &OperationId,
        publication: &PublicationReference,
    ) -> Result<(), LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO publications VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.as_str(),
                publication.repository,
                publication.branch,
                publication.commit_sha,
                publication.pull_request_url,
                publication.ci_sha
            ],
        )?;
        Ok(())
    }
    pub fn recover(&self) -> Result<Vec<RecoveryRecord>, LedgerError> {
        let mut connection = self.connection.lock().expect("ledger mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare("SELECT id, status FROM operations WHERE status IN ('accepted','running','interrupted')")?;
        let ids = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut records = Vec::new();
        for item in ids {
            let (id, status) = item?;
            let status = OperationStatus::from_str(&status)?;
            transaction.execute("UPDATE operations SET status='recovery_required' WHERE id=?1 AND status NOT IN ('succeeded','failed','invalid_output','timed_out','cancelled')", params![id])?;
            records.push(RecoveryRecord {
                operation: OperationId::new(id),
                status: OperationStatus::RecoveryRequired,
                reason: format!(
                    "operation was not terminal at restart (previously {})",
                    status.as_str()
                ),
            });
        }
        drop(statement);
        transaction.commit()?;
        Ok(records)
    }
    fn load(&self, id: &OperationId) -> Result<Option<OperationRecord>, LedgerError> {
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        connection.query_row("SELECT id, request_id, task_id, task_revision, payload, instruction, status, requested_provider, requested_model, accepted_at, started_at, finished_at, observed_provider, observed_model, diagnostic, artifact_ref FROM operations WHERE id=?1", params![id.as_str()], OperationRecord::from_row).optional().map_err(Into::into)
    }
}

fn safe_log_component(value: &str) -> Result<&str, LedgerError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(LedgerError::InvalidValue(format!(
            "unsafe log path component: {value:?}"
        )));
    }
    Ok(value)
}

impl ExecutionLedger for SqliteExecutionLedger {
    fn accept_operation(&self, request: &OperationRequest) -> Result<OperationRecord, LedgerError> {
        let mut connection = self.connection.lock().expect("ledger mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = transaction.query_row("SELECT id, request_id, task_id, task_revision, payload, instruction, status, requested_provider, requested_model, accepted_at, started_at, finished_at, observed_provider, observed_model, diagnostic, artifact_ref FROM operations WHERE request_id=?1", params![request.request_id()], OperationRecord::from_row).optional()? {
            if existing.request.payload() == request.payload()
                && existing.request.instruction() == request.instruction()
                && existing.request.task_id() == request.task_id()
                && existing.request.task_revision() == request.task_revision()
                && existing.request.requested_provider() == request.requested_provider()
                && existing.request.requested_model() == request.requested_model()
            {
                return Ok(existing);
            }
            return Err(LedgerError::RequestConflict { request_id: request.request_id().into() });
        }
        let actual: u64 = transaction
            .query_row(
                "SELECT revision FROM task_revisions WHERE task_id=?1",
                params![request.task_id().as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(request.task_revision() as i64) as u64;
        if actual != request.task_revision() {
            return Err(LedgerError::StaleRevision {
                expected: request.task_revision(),
                actual,
            });
        }
        let active: Option<String> = transaction.query_row("SELECT id FROM operations WHERE task_id=?1 AND status IN ('accepted','running','interrupted','recovery_required')", params![request.task_id().as_str()], |row| row.get(0)).optional()?;
        if let Some(id) = active {
            return Err(LedgerError::ActiveOperation(OperationId::new(id)));
        }
        let row_id: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(rowid), 0) + 1 FROM operations",
            [],
            |row| row.get(0),
        )?;
        let id = OperationId::new(format!(
            "op-{}-{row_id}-{}",
            request.task_id().as_str(),
            now()
        ));
        let accepted = now();
        transaction.execute("INSERT INTO operations(id,request_id,task_id,task_revision,payload,instruction,requested_provider,requested_model,status,accepted_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'accepted',?9)", params![id.as_str(), request.request_id(), request.task_id().as_str(), request.task_revision() as i64, request.payload(), request.instruction(), request.requested_provider().as_str(), request.requested_model(), accepted])?;
        transaction.execute(
            "INSERT INTO operation_events VALUES(?1,0,'accepted',?2,?3)",
            params![id.as_str(), accepted, request.instruction()],
        )?;
        transaction.commit()?;
        drop(connection);
        self.load(&id)?
            .ok_or_else(|| LedgerError::InvalidValue("operation insert disappeared".into()))
    }
    fn start_operation(
        &self,
        id: &OperationId,
        observed_provider: &ProviderRef,
        observed_model: Option<&str>,
    ) -> Result<(), LedgerError> {
        SqliteExecutionLedger::start_operation(self, id, observed_provider, observed_model)
    }
    fn get_operation(&self, id: &OperationId) -> Result<Option<OperationRecord>, LedgerError> {
        self.load(id)
    }
    fn finish_operation(
        &self,
        id: &OperationId,
        status: OperationStatus,
        diagnostic: Option<&str>,
    ) -> Result<(), LedgerError> {
        if !status.terminal() {
            return Err(LedgerError::InvalidValue(format!(
                "operation status is not terminal: {}",
                status.as_str()
            )));
        }
        let mut connection = self.connection.lock().expect("ledger mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT status FROM operations WHERE id=?1",
                params![id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(LedgerError::InvalidValue(format!(
                "unknown operation {}",
                id.as_str()
            )));
        };
        let current = OperationStatus::from_str(&current)?;
        if current.terminal() {
            if current != status {
                return Err(LedgerError::TerminalConflict(id.clone()));
            }
            return Ok(());
        }
        let finished = now();
        let changed = transaction.execute("UPDATE operations SET status=?2, finished_at=?3, diagnostic=COALESCE(?4, diagnostic) WHERE id=?1 AND status NOT IN ('succeeded','failed','invalid_output','timed_out','cancelled')", params![id.as_str(), status.as_str(), finished, diagnostic])?;
        if changed != 1 {
            return Err(LedgerError::TerminalConflict(id.clone()));
        }
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM operation_events WHERE operation_id=?1",
            params![id.as_str()],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO operation_events VALUES(?1,?2,'finished',?3,?4)",
            params![
                id.as_str(),
                sequence,
                finished,
                diagnostic.unwrap_or(status.as_str())
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(id: &str, payload: &str) -> OperationRequest {
        OperationRequest::new(
            id,
            TaskId::new("task-1"),
            3,
            payload,
            "implement",
            ProviderRef::new("codex"),
            Some("gpt".into()),
        )
    }
    #[test]
    fn idempotency_returns_same_operation_and_rejects_payload_change() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger.set_task_revision(&TaskId::new("task-1"), 3).unwrap();
        let first = ledger.accept_operation(&request("r1", "a")).unwrap();
        assert_eq!(
            ledger.accept_operation(&request("r1", "a")).unwrap().id(),
            first.id()
        );
        assert!(matches!(
            ledger.accept_operation(&request("r1", "b")),
            Err(LedgerError::RequestConflict { .. })
        ));
    }
    #[test]
    fn terminal_fact_cannot_be_overwritten() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let operation = ledger.accept_operation(&request("r1", "a")).unwrap();
        ledger
            .finish_operation(operation.id(), OperationStatus::TimedOut, Some("timeout"))
            .unwrap();
        assert!(matches!(
            ledger.finish_operation(operation.id(), OperationStatus::Succeeded, None),
            Err(LedgerError::TerminalConflict(_))
        ));
    }
    #[test]
    fn request_settings_are_part_of_idempotency() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let first = ledger.accept_operation(&request("r1", "a")).unwrap();
        let different_provider = OperationRequest::new(
            "r1",
            TaskId::new("task-1"),
            3,
            "a",
            "implement",
            ProviderRef::new("copilot"),
            Some("gpt".into()),
        );
        assert!(matches!(
            ledger.accept_operation(&different_provider),
            Err(LedgerError::RequestConflict { .. })
        ));
        assert_eq!(
            ledger.get_operation(first.id()).unwrap().unwrap().id(),
            first.id()
        );
    }
    #[test]
    fn start_requires_an_accepted_operation_and_terminal_finish_is_idempotent() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let unknown = OperationId::new("missing");
        assert!(matches!(
            ledger.start_operation(&unknown, &ProviderRef::new("codex"), Some("gpt")),
            Err(LedgerError::InvalidValue(_))
        ));
        let operation = ledger.accept_operation(&request("r1", "a")).unwrap();
        ledger
            .start_operation(operation.id(), &ProviderRef::new("codex"), Some("gpt"))
            .unwrap();
        ledger
            .finish_operation(operation.id(), OperationStatus::Succeeded, None)
            .unwrap();
        ledger
            .finish_operation(operation.id(), OperationStatus::Succeeded, None)
            .unwrap();
        assert_eq!(ledger.events(operation.id()).unwrap().len(), 3);
    }
    #[test]
    fn recovery_marks_unknown_state_without_claiming_success() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let operation = ledger.accept_operation(&request("r1", "a")).unwrap();
        let recovered = ledger.recover().unwrap();
        assert_eq!(recovered[0].operation, *operation.id());
        assert_eq!(
            ledger
                .get_operation(operation.id())
                .unwrap()
                .unwrap()
                .status(),
            OperationStatus::RecoveryRequired
        );
    }
    #[test]
    fn raw_logs_are_bounded_and_referenced() {
        let mut ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger.set_log_limit(3);
        let operation = ledger.accept_operation(&request("r1", "a")).unwrap();
        let reference = ledger
            .save_log(
                operation.id(),
                "stdout",
                std::env::temp_dir().join(format!("ledger-test-{}", now())),
                b"abcdef",
            )
            .unwrap();
        assert_eq!(reference.byte_count(), 3);
        assert!(reference.truncated());
        assert_eq!(fs::read(reference.path()).unwrap(), b"abc");
        let _ = fs::remove_file(reference.path());
    }

    #[test]
    fn save_log_rejects_path_traversal_components() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let directory = std::env::temp_dir().join(format!("ledger-path-test-{}", now()));
        assert!(matches!(
            ledger.save_log(&OperationId::new("../escape"), "stdout", &directory, b"x"),
            Err(LedgerError::InvalidValue(_))
        ));
        assert!(matches!(
            ledger.save_log(&OperationId::new("op-1"), "../stderr", &directory, b"x"),
            Err(LedgerError::InvalidValue(_))
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn schema_v1_upgrade_preserves_existing_operation_records() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE operations (id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL, task_revision INTEGER NOT NULL, payload TEXT NOT NULL, instruction TEXT NOT NULL, requested_provider TEXT NOT NULL, requested_model TEXT, status TEXT NOT NULL, accepted_at INTEGER NOT NULL, started_at INTEGER, finished_at INTEGER, observed_provider TEXT, observed_model TEXT, diagnostic TEXT, artifact_ref TEXT);
                 INSERT INTO operations VALUES ('op-1', 'request-1', 'task-1', 3, 'payload', 'instruction', 'codex', 'gpt', 'accepted', 10, NULL, NULL, NULL, NULL, NULL, NULL);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        let ledger = SqliteExecutionLedger::from_connection(connection).unwrap();
        let operation = ledger
            .get_operation(&OperationId::new("op-1"))
            .unwrap()
            .unwrap();
        assert_eq!(operation.request().task_id(), &TaskId::new("task-1"));
        assert_eq!(operation.request().payload(), "payload");
        let version: u32 = ledger
            .connection
            .lock()
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
    }
}
