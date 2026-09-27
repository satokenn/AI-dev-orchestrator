use std::{
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{ProviderRef, TaskId};

const SCHEMA_VERSION: u32 = 2;
const DEFAULT_LOG_LIMIT: usize = 1024 * 1024;
static LOG_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn initialize_schema(connection: &mut Connection) -> Result<(), LedgerError> {
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(LedgerError::UnsupportedSchema(version));
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, request_id TEXT NOT NULL UNIQUE, task_id TEXT NOT NULL, task_revision INTEGER NOT NULL, payload TEXT NOT NULL, instruction TEXT NOT NULL, requested_provider TEXT NOT NULL, requested_model TEXT, status TEXT NOT NULL, accepted_at INTEGER NOT NULL, started_at INTEGER, finished_at INTEGER, observed_provider TEXT, observed_model TEXT, diagnostic TEXT, artifact_ref TEXT);
         CREATE TABLE IF NOT EXISTS task_revisions (task_id TEXT PRIMARY KEY, revision INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS operation_events (operation_id TEXT NOT NULL, sequence INTEGER NOT NULL, kind TEXT NOT NULL, occurred_at INTEGER NOT NULL, detail TEXT NOT NULL, PRIMARY KEY(operation_id, sequence));
         CREATE TABLE IF NOT EXISTS log_references (operation_id TEXT NOT NULL, stream TEXT NOT NULL, path TEXT NOT NULL, byte_count INTEGER NOT NULL, truncated INTEGER NOT NULL, PRIMARY KEY(operation_id, stream));
         CREATE TABLE IF NOT EXISTS validations (operation_id TEXT PRIMARY KEY, artifact_ref TEXT NOT NULL, passed INTEGER NOT NULL, summary TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS reviews (operation_id TEXT PRIMARY KEY, artifact_ref TEXT NOT NULL, verdict TEXT NOT NULL, summary TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS usage (operation_id TEXT PRIMARY KEY, input_units TEXT, output_units TEXT, cost TEXT, currency TEXT);
         CREATE TABLE IF NOT EXISTS budget_reservations (operation_id TEXT PRIMARY KEY, amount TEXT NOT NULL, settled_amount TEXT, state TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS publications (operation_id TEXT PRIMARY KEY, repository TEXT NOT NULL, branch TEXT, commit_sha TEXT, pull_request_url TEXT, ci_sha TEXT);",
    )?;
    if version < 2 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (table, columns, definition) in [
            (
                "operation_events",
                "operation_id, sequence, kind, occurred_at, detail",
                "operation_id TEXT NOT NULL REFERENCES operations(id), sequence INTEGER NOT NULL, kind TEXT NOT NULL, occurred_at INTEGER NOT NULL, detail TEXT NOT NULL, PRIMARY KEY(operation_id, sequence)",
            ),
            (
                "log_references",
                "operation_id, stream, path, byte_count, truncated",
                "operation_id TEXT NOT NULL REFERENCES operations(id), stream TEXT NOT NULL, path TEXT NOT NULL, byte_count INTEGER NOT NULL, truncated INTEGER NOT NULL, PRIMARY KEY(operation_id, stream)",
            ),
            (
                "validations",
                "operation_id, artifact_ref, passed, summary",
                "operation_id TEXT PRIMARY KEY REFERENCES operations(id), artifact_ref TEXT NOT NULL, passed INTEGER NOT NULL, summary TEXT NOT NULL",
            ),
            (
                "reviews",
                "operation_id, artifact_ref, verdict, summary",
                "operation_id TEXT PRIMARY KEY REFERENCES operations(id), artifact_ref TEXT NOT NULL, verdict TEXT NOT NULL, summary TEXT NOT NULL",
            ),
            (
                "usage",
                "operation_id, input_units, output_units, cost, currency",
                "operation_id TEXT PRIMARY KEY REFERENCES operations(id), input_units TEXT, output_units TEXT, cost TEXT, currency TEXT",
            ),
            (
                "budget_reservations",
                "operation_id, amount, settled_amount, state",
                "operation_id TEXT PRIMARY KEY REFERENCES operations(id), amount TEXT NOT NULL, settled_amount TEXT, state TEXT NOT NULL",
            ),
            (
                "publications",
                "operation_id, repository, branch, commit_sha, pull_request_url, ci_sha",
                "operation_id TEXT PRIMARY KEY REFERENCES operations(id), repository TEXT NOT NULL, branch TEXT, commit_sha TEXT, pull_request_url TEXT, ci_sha TEXT",
            ),
        ] {
            let replacement = format!("{table}_v2");
            transaction.execute_batch(&format!(
                "CREATE TABLE {replacement} ({definition}); INSERT INTO {replacement} ({columns}) SELECT {columns} FROM {table}; DROP TABLE {table}; ALTER TABLE {replacement} RENAME TO {table};"
            ))?;
        }
        transaction.pragma_update(None, "user_version", 2)?;
        transaction.commit()?;
    }
    Ok(())
}

/// Exclusive process lock for one canonical operation ledger.
pub struct LedgerRunLock {
    _file: std::fs::File,
    _operation_file: std::fs::File,
    ledger_path: PathBuf,
    operation_path: PathBuf,
}

impl LedgerRunLock {
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let canonical = canonical_ledger_path(path.as_ref())?;
        let operation_path = operation_database_path(&canonical)?;
        let (lock_path, operation_lock_path) = ledger_run_lock_paths(&canonical, &operation_path)?;
        let file = open_exclusive_lock(&lock_path)?;
        let operation_file = open_exclusive_lock(&operation_lock_path)?;
        Ok(Self {
            _file: file,
            _operation_file: operation_file,
            ledger_path: canonical,
            operation_path,
        })
    }
    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }
}

fn ledger_run_lock_paths(
    ledger_path: &Path,
    operation_path: &Path,
) -> Result<(PathBuf, PathBuf), LedgerError> {
    let mut name = ledger_path
        .file_name()
        .ok_or_else(|| LedgerError::InvalidValue("ledger path has no file name".into()))?
        .to_os_string();
    name.push(".operations.lock");
    let lock_path = ledger_path.with_file_name(name);
    let mut operation_lock = operation_path.as_os_str().to_os_string();
    operation_lock.push(".recovery.lock");
    Ok((lock_path, PathBuf::from(operation_lock)))
}

fn open_exclusive_lock(path: &Path) -> Result<std::fs::File, LedgerError> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock_exclusive().map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            LedgerError::LockBusy
        } else {
            LedgerError::Io(error)
        }
    })?;
    Ok(file)
}

fn canonical_ledger_path(path: &Path) -> Result<PathBuf, LedgerError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let name = path
        .file_name()
        .ok_or_else(|| LedgerError::InvalidValue("ledger path has no file name".into()))?;
    let candidate = parent.join(name);
    let canonical = match fs::symlink_metadata(&candidate) {
        Ok(_) => fs::canonicalize(candidate)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => candidate,
        Err(e) => return Err(e.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match fs::metadata(&canonical) {
            Ok(metadata) if metadata.nlink() > 1 => {
                return Err(LedgerError::InvalidValue(
                    "hard-linked ledger files are not supported".into(),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(canonical)
}

fn operation_database_path(ledger_path: &Path) -> Result<PathBuf, LedgerError> {
    let mut name = ledger_path.as_os_str().to_os_string();
    name.push(".operations.sqlite3");
    let candidate = PathBuf::from(name);
    let canonical = match fs::symlink_metadata(&candidate) {
        Ok(_) => fs::canonicalize(&candidate)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => candidate,
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match fs::metadata(&canonical) {
            Ok(metadata) if metadata.nlink() > 1 => {
                return Err(LedgerError::InvalidValue(
                    "hard-linked operation ledger files are not supported".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(canonical)
}

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
    BudgetReservationConflict(OperationId),
    BudgetNotReserved(OperationId),
    BudgetSettlementConflict(OperationId),
    UnsupportedSchema(u32),
    RecoveryLockRequired,
    RecoveryLockMismatch,
    LockBusy,
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
            Self::BudgetReservationConflict(id) => write!(
                f,
                "budget reservation conflicts with existing reservation: {}",
                id.as_str()
            ),
            Self::BudgetNotReserved(id) => {
                write!(f, "operation has no reserved budget: {}", id.as_str())
            }
            Self::BudgetSettlementConflict(id) => write!(
                f,
                "budget reservation was settled with a different amount: {}",
                id.as_str()
            ),
            Self::UnsupportedSchema(v) => write!(f, "unsupported ledger schema: {v}"),
            Self::RecoveryLockRequired => write!(f, "recovery requires an exclusive ledger lock"),
            Self::RecoveryLockMismatch => write!(f, "recovery lock belongs to a different ledger"),
            Self::LockBusy => write!(f, "ledger is busy: another process is running it"),
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
    fn mark_recovery_required(&self, id: &OperationId, diagnostic: &str)
    -> Result<(), LedgerError>;
}

pub struct SqliteExecutionLedger {
    connection: Mutex<Connection>,
    log_limit: usize,
    ledger_path: Option<PathBuf>,
    operation_path: Option<PathBuf>,
}

impl SqliteExecutionLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let ledger_path = canonical_ledger_path(path.as_ref())?;
        let operation_path = operation_database_path(&ledger_path)?;
        let mut ledger = Self::from_connection(Connection::open(&operation_path)?)?;
        ledger.ledger_path = Some(ledger_path);
        ledger.operation_path = Some(operation_path);
        Ok(ledger)
    }
    pub fn open_in_memory() -> Result<Self, LedgerError> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    fn from_connection(mut connection: Connection) -> Result<Self, LedgerError> {
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(LedgerError::UnsupportedSchema(version));
        }
        initialize_schema(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            log_limit: DEFAULT_LOG_LIMIT,
            ledger_path: None,
            operation_path: None,
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
        let connection = self.connection.lock().expect("ledger mutex poisoned");
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1)",
            params![id.as_str()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(LedgerError::InvalidValue(format!(
                "unknown operation: {}",
                id.as_str()
            )));
        }
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
        let log_content = &content[..limit];
        let existing: Option<(String, i64, bool)> = connection
            .query_row(
                "SELECT path, byte_count, truncated FROM log_references WHERE operation_id=?1 AND stream=?2",
                params![id.as_str(), stream],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((stored_path, byte_count, stored_truncated)) = existing {
            let stored_path = PathBuf::from(stored_path);
            let metadata = fs::symlink_metadata(&stored_path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(LedgerError::InvalidValue(
                    "stored log path is not a regular file".into(),
                ));
            }
            if stored_path == path
                && byte_count == limit as i64
                && stored_truncated == truncated
                && fs::read(&stored_path)? == log_content
            {
                return Ok(LogReference {
                    path: stored_path,
                    byte_count: limit as u64,
                    truncated,
                });
            }
            return Err(LedgerError::InvalidValue(
                "log reference already exists with different content".into(),
            ));
        }
        let (created_identity, created_in_this_call) = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(LedgerError::InvalidValue(
                        "log path already exists and is not a regular file".into(),
                    ));
                }
                if fs::read(&path)? != log_content {
                    return Err(LedgerError::InvalidValue(
                        "log path already exists with different content".into(),
                    ));
                }
                // A previous call may have completed the file write but failed before
                // registering its reference. Adopt only the exact requested bytes.
                (metadata, false)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (temporary_path, mut file) = create_log_staging_file(
                    &canonical_directory,
                    operation_component,
                    stream_component,
                )?;
                let temporary_identity = file.metadata()?;
                if let Err(error) = file.write_all(log_content) {
                    drop(file);
                    remove_staging_log_if_unchanged(
                        &temporary_path,
                        &temporary_identity,
                        log_content,
                    );
                    return Err(error.into());
                }
                file.sync_all()?;
                drop(file);
                let publish_result = fs::hard_link(&temporary_path, &path);
                remove_staging_log_if_unchanged(&temporary_path, &temporary_identity, log_content);
                publish_result?;
                (fs::symlink_metadata(&path)?, true)
            }
            Err(error) => return Err(error.into()),
        };
        let reference = LogReference {
            path,
            byte_count: limit as u64,
            truncated,
        };
        let insert_result = connection.execute(
            "INSERT INTO log_references VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                id.as_str(),
                stream,
                reference.path.to_string_lossy().as_ref(),
                reference.byte_count as i64,
                reference.truncated
            ],
        );
        if let Err(error) = insert_result {
            if created_in_this_call {
                remove_created_log_if_unchanged(reference.path(), &created_identity);
            }
            return Err(error.into());
        }
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
        let mut connection = self.connection.lock().expect("ledger mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, Option<String>)> = transaction
            .query_row(
                "SELECT amount, state, settled_amount FROM budget_reservations WHERE operation_id=?1",
                params![id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((existing_amount, state, settled)) = existing {
            if existing_amount == amount
                && ((state == "reserved" && settled.is_none())
                    || (state == "settled" && settled.is_some()))
            {
                transaction.commit()?;
                return Ok(());
            }
            return Err(LedgerError::BudgetReservationConflict(id.clone()));
        }
        transaction.execute(
            "INSERT INTO budget_reservations VALUES(?1, ?2, NULL, 'reserved')",
            params![id.as_str(), amount],
        )?;
        transaction.commit()?;
        Ok(())
    }
    pub fn settle_budget(&self, id: &OperationId, amount: &str) -> Result<(), LedgerError> {
        let mut connection = self.connection.lock().expect("ledger mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, Option<String>)> = transaction
            .query_row(
                "SELECT state, settled_amount FROM budget_reservations WHERE operation_id=?1",
                params![id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match existing {
            Some((state, Some(settled))) if state == "settled" && settled == amount => {
                transaction.commit()?;
                return Ok(());
            }
            Some((state, _)) if state == "reserved" => {
                transaction.execute(
                    "UPDATE budget_reservations SET settled_amount=?2, state='settled' WHERE operation_id=?1",
                    params![id.as_str(), amount],
                )?;
            }
            Some(_) => return Err(LedgerError::BudgetSettlementConflict(id.clone())),
            None => return Err(LedgerError::BudgetNotReserved(id.clone())),
        }
        transaction.commit()?;
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
    pub fn recover(&self, lock: &LedgerRunLock) -> Result<Vec<RecoveryRecord>, LedgerError> {
        let expected = self
            .ledger_path
            .as_ref()
            .ok_or(LedgerError::RecoveryLockRequired)?;
        if expected != &lock.ledger_path {
            return Err(LedgerError::RecoveryLockMismatch);
        }
        if self.operation_path.as_ref() != Some(&lock.operation_path) {
            return Err(LedgerError::RecoveryLockMismatch);
        }
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
            let reason = format!(
                "operation was not terminal at restart (previously {})",
                status.as_str()
            );
            let recovered_at = now();
            let changed = transaction.execute(
                "UPDATE operations SET status='recovery_required', diagnostic=?2 \
                 WHERE id=?1 AND status IN ('accepted','running','interrupted')",
                params![id, reason],
            )?;
            if changed != 1 {
                return Err(LedgerError::InvalidValue(format!(
                    "operation changed during recovery: {id}"
                )));
            }
            let sequence: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(sequence), -1) + 1 FROM operation_events WHERE operation_id=?1",
                params![id],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO operation_events VALUES(?1, ?2, 'recovery', ?3, ?4)",
                params![id, sequence, recovered_at, reason],
            )?;
            records.push(RecoveryRecord {
                operation: OperationId::new(id),
                status: OperationStatus::RecoveryRequired,
                reason,
            });
        }
        drop(statement);
        transaction.commit()?;
        Ok(records)
    }
    pub fn mark_recovery_required(
        &self,
        id: &OperationId,
        diagnostic: &str,
    ) -> Result<(), LedgerError> {
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
        if current == OperationStatus::RecoveryRequired {
            let saved_diagnostic: Option<String> = transaction.query_row(
                "SELECT diagnostic FROM operations WHERE id=?1",
                params![id.as_str()],
                |row| row.get(0),
            )?;
            return if saved_diagnostic.as_deref() == Some(diagnostic) {
                Ok(())
            } else {
                Err(LedgerError::TerminalConflict(id.clone()))
            };
        }
        if !matches!(
            current,
            OperationStatus::Accepted | OperationStatus::Running | OperationStatus::Interrupted
        ) {
            return Err(LedgerError::TerminalConflict(id.clone()));
        }

        let occurred_at = now();
        let changed = transaction.execute(
            "UPDATE operations SET status='recovery_required', diagnostic=?2 \
             WHERE id=?1 AND status IN ('accepted','running','interrupted')",
            params![id.as_str(), diagnostic],
        )?;
        if changed != 1 {
            return Err(LedgerError::TerminalConflict(id.clone()));
        }
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM operation_events WHERE operation_id=?1",
            params![id.as_str()],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO operation_events VALUES(?1, ?2, 'recovery', ?3, ?4)",
            params![id.as_str(), sequence, occurred_at, diagnostic],
        )?;
        transaction.commit()?;
        Ok(())
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

fn create_log_staging_file(
    directory: &Path,
    operation: &str,
    stream: &str,
) -> Result<(PathBuf, fs::File), LedgerError> {
    for _ in 0..32 {
        let sequence = LOG_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".{operation}-{stream}-{}-{sequence}.tmp",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(LedgerError::InvalidValue(
        "could not allocate a unique log staging file".into(),
    ))
}

#[cfg(unix)]
fn remove_created_log_if_unchanged(path: &Path, created: &fs::Metadata) {
    use std::os::unix::fs::MetadataExt;

    if let Ok(current) = fs::symlink_metadata(path)
        && current.dev() == created.dev()
        && current.ino() == created.ino()
    {
        let _ = fs::remove_file(path);
    }
}

#[cfg(not(unix))]
fn remove_created_log_if_unchanged(_path: &Path, _created: &fs::Metadata) {}

#[cfg(unix)]
fn remove_staging_log_if_unchanged(path: &Path, created: &fs::Metadata, _content: &[u8]) {
    remove_created_log_if_unchanged(path, created);
}

#[cfg(not(unix))]
fn remove_staging_log_if_unchanged(path: &Path, _created: &fs::Metadata, content: &[u8]) {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && !metadata.file_type().is_symlink()
        && metadata.is_file()
        && fs::read(path).is_ok_and(|staged| staged == content)
    {
        let _ = fs::remove_file(path);
    }
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
        if matches!(
            current,
            OperationStatus::Interrupted | OperationStatus::RecoveryRequired
        ) {
            return Err(LedgerError::TerminalConflict(id.clone()));
        }
        if current.terminal() {
            if current != status {
                return Err(LedgerError::TerminalConflict(id.clone()));
            }
            return Ok(());
        }
        let finished = now();
        let changed = transaction.execute("UPDATE operations SET status=?2, finished_at=?3, diagnostic=COALESCE(?4, diagnostic) WHERE id=?1 AND status IN ('accepted','running')", params![id.as_str(), status.as_str(), finished, diagnostic])?;
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
    fn mark_recovery_required(
        &self,
        id: &OperationId,
        diagnostic: &str,
    ) -> Result<(), LedgerError> {
        SqliteExecutionLedger::mark_recovery_required(self, id, diagnostic)
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

    fn recovery_test_path(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "operation-recovery-{label}-{}-{}.sqlite3",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }
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
        let path = recovery_test_path("state");
        let ledger = SqliteExecutionLedger::open(&path).unwrap();
        let operation = ledger.accept_operation(&request("r1", "a")).unwrap();
        let lock = LedgerRunLock::acquire(&path).unwrap();
        let recovered = ledger.recover(&lock).unwrap();
        assert_eq!(recovered[0].operation, *operation.id());
        assert_eq!(
            recovered[0].reason,
            "operation was not terminal at restart (previously accepted)"
        );
        assert_eq!(
            ledger
                .get_operation(operation.id())
                .unwrap()
                .unwrap()
                .status(),
            OperationStatus::RecoveryRequired
        );
        assert_eq!(
            ledger
                .get_operation(operation.id())
                .unwrap()
                .unwrap()
                .diagnostic(),
            Some(recovered[0].reason.as_str())
        );
        let events = ledger.events(operation.id()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, EventKind::Recovery);
        assert_eq!(events[1].detail, recovered[0].reason);
        assert!(ledger.recover(&lock).unwrap().is_empty());
        assert_eq!(ledger.events(operation.id()).unwrap().len(), 2);
        drop(lock);
        drop(ledger);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}.operations.sqlite3", path.display()));
        let _ = fs::remove_file(format!("{}.operations.lock", path.display()));
    }

    #[test]
    fn recovery_requires_a_lock_for_the_same_canonical_ledger() {
        let first_path = recovery_test_path("first");
        let second_path = recovery_test_path("second");
        let ledger = SqliteExecutionLedger::open(&first_path).unwrap();
        let operation = ledger.accept_operation(&request("r1", "a")).unwrap();
        let wrong_lock = LedgerRunLock::acquire(&second_path).unwrap();
        assert!(matches!(
            ledger.recover(&wrong_lock),
            Err(LedgerError::RecoveryLockMismatch)
        ));
        assert_eq!(
            ledger
                .get_operation(operation.id())
                .unwrap()
                .unwrap()
                .status(),
            OperationStatus::Accepted
        );
        drop(wrong_lock);
        let lock = LedgerRunLock::acquire(&first_path).unwrap();
        assert_eq!(ledger.recover(&lock).unwrap().len(), 1);
        drop(lock);
        drop(ledger);
        for path in [&first_path, &second_path] {
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(format!("{}.operations.sqlite3", path.display()));
            let _ = fs::remove_file(format!("{}.operations.lock", path.display()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_ledger_names_keep_sidecars_and_locks_distinct() {
        use std::os::unix::ffi::OsStringExt;

        let root = std::env::temp_dir().join(format!("operation-non-utf8-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let first_path = root.join(std::ffi::OsString::from_vec(b"ledger-\xff".to_vec()));
        let second_path = root.join(std::ffi::OsString::from_vec(b"ledger-\xfe".to_vec()));

        let first_canonical = canonical_ledger_path(&first_path).unwrap();
        let second_canonical = canonical_ledger_path(&second_path).unwrap();
        let first_sidecar = operation_database_path(&first_canonical).unwrap();
        let second_sidecar = operation_database_path(&second_canonical).unwrap();
        let (first_lock, first_operation_lock) =
            ledger_run_lock_paths(&first_canonical, &first_sidecar).unwrap();
        let (second_lock, second_operation_lock) =
            ledger_run_lock_paths(&second_canonical, &second_sidecar).unwrap();
        use std::os::unix::ffi::OsStrExt;
        assert_ne!(
            first_sidecar.as_os_str().as_bytes(),
            second_sidecar.as_os_str().as_bytes()
        );
        assert_ne!(
            first_lock.as_os_str().as_bytes(),
            second_lock.as_os_str().as_bytes()
        );
        assert_ne!(
            first_operation_lock.as_os_str().as_bytes(),
            second_operation_lock.as_os_str().as_bytes()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_operation_sidecar_is_rejected() {
        let root =
            std::env::temp_dir().join(format!("operation-sidecar-hardlink-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let first_path = root.join("first.sqlite3");
        let second_path = root.join("second.sqlite3");
        drop(SqliteExecutionLedger::open(&first_path).unwrap());
        let mut first_sidecar = first_path.as_os_str().to_os_string();
        first_sidecar.push(".operations.sqlite3");
        let mut second_sidecar = second_path.as_os_str().to_os_string();
        second_sidecar.push(".operations.sqlite3");
        fs::hard_link(PathBuf::from(first_sidecar), PathBuf::from(second_sidecar)).unwrap();

        assert!(matches!(
            LedgerRunLock::acquire(&first_path),
            Err(LedgerError::InvalidValue(message)) if message.contains("hard-linked operation ledger")
        ));
        assert!(matches!(
            SqliteExecutionLedger::open(&second_path),
            Err(LedgerError::InvalidValue(message)) if message.contains("hard-linked operation ledger")
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_operation_sidecar_uses_the_target_lock() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("operation-sidecar-symlink-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let first_path = root.join("first.sqlite3");
        let second_path = root.join("second.sqlite3");
        drop(SqliteExecutionLedger::open(&first_path).unwrap());
        let mut first_sidecar = first_path.as_os_str().to_os_string();
        first_sidecar.push(".operations.sqlite3");
        let mut second_sidecar = second_path.as_os_str().to_os_string();
        second_sidecar.push(".operations.sqlite3");
        let first_sidecar = PathBuf::from(first_sidecar);
        symlink(&first_sidecar, PathBuf::from(second_sidecar)).unwrap();

        let first_lock = LedgerRunLock::acquire(&first_path).unwrap();
        assert!(matches!(
            LedgerRunLock::acquire(&second_path),
            Err(LedgerError::LockBusy)
        ));
        drop(first_lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_required_operation_cannot_be_finished_or_overwritten() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let operation = ledger.accept_operation(&request("r1", "a")).unwrap();
        ledger
            .start_operation(operation.id(), &ProviderRef::new("codex"), Some("gpt"))
            .unwrap();
        ledger
            .mark_recovery_required(operation.id(), "process outcome is unknown")
            .unwrap();
        ledger
            .mark_recovery_required(operation.id(), "process outcome is unknown")
            .unwrap();

        assert!(matches!(
            ledger.finish_operation(operation.id(), OperationStatus::Succeeded, None),
            Err(LedgerError::TerminalConflict(_))
        ));
        assert!(matches!(
            ledger.mark_recovery_required(operation.id(), "different diagnosis"),
            Err(LedgerError::TerminalConflict(_))
        ));
        let stored = ledger.get_operation(operation.id()).unwrap().unwrap();
        assert_eq!(stored.status(), OperationStatus::RecoveryRequired);
        assert_eq!(stored.finished_at(), None);
        assert_eq!(stored.diagnostic(), Some("process outcome is unknown"));
        let events = ledger.events(operation.id()).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2].kind, EventKind::Recovery);
        assert_eq!(events[2].detail, "process outcome is unknown");
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
        let replay = ledger
            .save_log(
                operation.id(),
                "stdout",
                reference.path().parent().unwrap(),
                b"abcdef",
            )
            .unwrap();
        assert_eq!(replay.path(), reference.path());
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
    fn save_log_rejects_unknown_operation_before_creating_files() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let directory = std::env::temp_dir().join(format!("ledger-missing-log-{}", now()));
        assert!(matches!(
            ledger.save_log(&OperationId::new("missing"), "stdout", &directory, b"x"),
            Err(LedgerError::InvalidValue(_))
        ));
        assert!(!directory.exists());
    }

    #[test]
    fn save_log_recovers_an_exact_unreferenced_file() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let operation = ledger
            .accept_operation(&request("r-log-orphan", "a"))
            .unwrap();
        let directory = std::env::temp_dir().join(format!("ledger-log-orphan-{}", now()));
        fs::create_dir_all(&directory).unwrap();
        let orphan = directory.join(format!("{}-stdout.log", operation.id().as_str()));
        fs::write(&orphan, b"complete log").unwrap();

        let reference = ledger
            .save_log(operation.id(), "stdout", &directory, b"complete log")
            .unwrap();
        assert_eq!(reference.path(), orphan.canonicalize().unwrap());
        assert_eq!(fs::read(reference.path()).unwrap(), b"complete log");
        assert!(
            ledger
                .save_log(operation.id(), "stdout", &directory, b"different log")
                .is_err()
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn save_log_does_not_follow_a_preexisting_symlink() {
        use std::os::unix::fs::symlink;

        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let operation = ledger.accept_operation(&request("r-symlink", "a")).unwrap();
        let directory = std::env::temp_dir().join(format!("ledger-symlink-{}", now()));
        fs::create_dir_all(&directory).unwrap();
        let target = directory.join("outside-target");
        fs::write(&target, b"keep").unwrap();
        let log_path = directory.join(format!("{}-stdout.log", operation.id().as_str()));
        symlink(&target, &log_path).unwrap();

        assert!(
            ledger
                .save_log(operation.id(), "stdout", &directory, b"overwrite")
                .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        assert!(
            fs::symlink_metadata(&log_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn save_log_removes_its_new_file_when_reference_insert_fails() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let operation = ledger
            .accept_operation(&request("r-log-insert", "a"))
            .unwrap();
        let directory = std::env::temp_dir().join(format!("ledger-log-insert-{}", now()));
        ledger
            .connection
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_log_insert BEFORE INSERT ON log_references BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;",
            )
            .unwrap();
        assert!(
            ledger
                .save_log(operation.id(), "stdout", &directory, b"partial")
                .is_err()
        );
        let orphan = directory.join(format!("{}-stdout.log", operation.id().as_str()));
        if orphan.exists() {
            assert_eq!(fs::read(&orphan).unwrap(), b"partial");
        }
        ledger
            .connection
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_log_insert;")
            .unwrap();
        let reference = ledger
            .save_log(operation.id(), "stdout", &directory, b"partial")
            .unwrap();
        assert_eq!(fs::read(reference.path()).unwrap(), b"partial");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn budget_reservation_and_settlement_are_idempotent_but_conflicts_fail() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let operation = ledger.accept_operation(&request("r-budget", "a")).unwrap();
        ledger.reserve_budget(operation.id(), "10").unwrap();
        ledger.reserve_budget(operation.id(), "10").unwrap();
        assert!(matches!(
            ledger.reserve_budget(operation.id(), "11"),
            Err(LedgerError::BudgetReservationConflict(_))
        ));
        ledger.settle_budget(operation.id(), "7").unwrap();
        ledger.settle_budget(operation.id(), "7").unwrap();
        // A retry of the original reservation can arrive after the settlement
        // committed if the reservation response was lost.
        ledger.reserve_budget(operation.id(), "10").unwrap();
        assert!(matches!(
            ledger.settle_budget(operation.id(), "8"),
            Err(LedgerError::BudgetSettlementConflict(_))
        ));
        assert!(matches!(
            ledger.settle_budget(&OperationId::new("missing"), "1"),
            Err(LedgerError::BudgetNotReserved(_))
        ));
    }

    #[test]
    fn budget_operations_are_serialized_across_ledger_connections() {
        let path = recovery_test_path("budget-connections");
        let first = SqliteExecutionLedger::open(&path).unwrap();
        let operation = first
            .accept_operation(&request("r-budget-connection", "a"))
            .unwrap();
        let second = SqliteExecutionLedger::open(&path).unwrap();
        first.reserve_budget(operation.id(), "10").unwrap();
        second.reserve_budget(operation.id(), "10").unwrap();
        assert!(matches!(
            second.reserve_budget(operation.id(), "11"),
            Err(LedgerError::BudgetReservationConflict(_))
        ));
        first.settle_budget(operation.id(), "7").unwrap();
        second.settle_budget(operation.id(), "7").unwrap();
        assert!(matches!(
            second.settle_budget(operation.id(), "8"),
            Err(LedgerError::BudgetSettlementConflict(_))
        ));
        drop(second);
        drop(first);
        for path in [
            path.clone(),
            PathBuf::from(format!("{}.operations.sqlite3", path.display())),
            PathBuf::from(format!("{}.operations.lock", path.display())),
        ] {
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn schema_v1_migration_preserves_all_children_and_adds_foreign_keys() {
        let path = recovery_test_path("migration-v1");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection.execute_batch(
            "CREATE TABLE operations (id TEXT PRIMARY KEY);
             INSERT INTO operations VALUES ('op-1');
             CREATE TABLE operation_events (operation_id TEXT NOT NULL, sequence INTEGER NOT NULL, kind TEXT NOT NULL, occurred_at INTEGER NOT NULL, detail TEXT NOT NULL, PRIMARY KEY(operation_id, sequence));
             CREATE TABLE log_references (operation_id TEXT NOT NULL, stream TEXT NOT NULL, path TEXT NOT NULL, byte_count INTEGER NOT NULL, truncated INTEGER NOT NULL, PRIMARY KEY(operation_id, stream));
             CREATE TABLE validations (operation_id TEXT PRIMARY KEY, artifact_ref TEXT NOT NULL, passed INTEGER NOT NULL, summary TEXT NOT NULL);
             CREATE TABLE reviews (operation_id TEXT PRIMARY KEY, artifact_ref TEXT NOT NULL, verdict TEXT NOT NULL, summary TEXT NOT NULL);
             CREATE TABLE usage (operation_id TEXT PRIMARY KEY, input_units TEXT, output_units TEXT, cost TEXT, currency TEXT);
             CREATE TABLE budget_reservations (operation_id TEXT PRIMARY KEY, amount TEXT NOT NULL, settled_amount TEXT, state TEXT NOT NULL);
             CREATE TABLE publications (operation_id TEXT PRIMARY KEY, repository TEXT NOT NULL, branch TEXT, commit_sha TEXT, pull_request_url TEXT, ci_sha TEXT);
             INSERT INTO operation_events VALUES ('op-1', 0, 'accepted', 42, 'kept');
             INSERT INTO log_references VALUES ('op-1', 'stdout', '/tmp/log', 4, 0);
             INSERT INTO validations VALUES ('op-1', 'tree-1', 1, 'passed');
             INSERT INTO reviews VALUES ('op-1', 'tree-1', 'approve', 'ok');
             INSERT INTO usage VALUES ('op-1', '2', '3', '0.1', 'USD');
             INSERT INTO budget_reservations VALUES ('op-1', '10', '7', 'settled');
             INSERT INTO publications VALUES ('op-1', 'repo', 'main', 'sha1', 'url', 'sha2');
             PRAGMA user_version = 1;",
        )
        .unwrap();
        initialize_schema(&mut connection).unwrap();
        drop(connection);

        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        initialize_schema(&mut connection).unwrap();
        for table in [
            "operation_events",
            "log_references",
            "validations",
            "reviews",
            "usage",
            "budget_reservations",
            "publications",
        ] {
            let count: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE operation_id='op-1'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "row was not preserved in {table}");
            let foreign_key_count: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_foreign_key_list('{table}') WHERE \"table\"='operations'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(foreign_key_count, 1, "missing FK on {table}");
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT kind, occurred_at, detail FROM operation_events WHERE operation_id='op-1' AND sequence=0",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)),
                )
                .unwrap(),
            ("accepted".into(), 42, "kept".into())
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT stream, path, byte_count, truncated FROM log_references WHERE operation_id='op-1'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, bool>(3)?)),
                )
                .unwrap(),
            ("stdout".into(), "/tmp/log".into(), 4, false)
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT artifact_ref, passed, summary FROM validations WHERE operation_id='op-1'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?, row.get::<_, String>(2)?)),
                )
                .unwrap(),
            ("tree-1".into(), true, "passed".into())
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT artifact_ref, verdict, summary FROM reviews WHERE operation_id='op-1'",
                    [],
                    |row| Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?
                    )),
                )
                .unwrap(),
            ("tree-1".into(), "approve".into(), "ok".into())
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT input_units, output_units, cost, currency FROM usage WHERE operation_id='op-1'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
                )
                .unwrap(),
            ("2".into(), "3".into(), "0.1".into(), "USD".into())
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT amount, settled_amount, state FROM budget_reservations WHERE operation_id='op-1'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
                )
                .unwrap(),
            ("10".into(), "7".into(), "settled".into())
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT repository, branch, commit_sha, pull_request_url, ci_sha FROM publications WHERE operation_id='op-1'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?)),
                )
                .unwrap(),
            ("repo".into(), Some("main".into()), Some("sha1".into()), Some("url".into()), Some("sha2".into()))
        );
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            2
        );
        for orphan_insert in [
            "INSERT INTO operation_events VALUES ('missing', 0, 'accepted', 43, 'orphan')",
            "INSERT INTO log_references VALUES ('missing', 'stdout', '/tmp/orphan', 1, 0)",
            "INSERT INTO validations VALUES ('missing', 'tree', 1, 'orphan')",
            "INSERT INTO reviews VALUES ('missing', 'tree', 'approve', 'orphan')",
            "INSERT INTO usage VALUES ('missing', NULL, NULL, NULL, NULL)",
            "INSERT INTO budget_reservations VALUES ('missing', '1', NULL, 'reserved')",
            "INSERT INTO publications VALUES ('missing', 'repo', NULL, NULL, NULL, NULL)",
        ] {
            assert!(connection.execute(orphan_insert, []).is_err());
        }
        drop(connection);
        for path in [
            path.clone(),
            PathBuf::from(format!("{}.operations.sqlite3", path.display())),
            PathBuf::from(format!("{}.operations.lock", path.display())),
        ] {
            let _ = fs::remove_file(path);
        }
    }
}
