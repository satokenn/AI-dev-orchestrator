//! Local persistence for tasks and their immutable attempt history.
//!
//! The ledger stores timestamps as Unix milliseconds. Timestamps are optional so
//! a queued attempt can be recorded before its provider starts.

use std::{collections::BTreeMap, fmt, path::Path, sync::Mutex};

use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::AttemptTargets;
use crate::{
    AgentResult, Attempt, AttemptFailureReason, AttemptId, AttemptSemantics, AttemptState,
    ModelChoice, ModelRef, ProviderRef, Task, TaskId, TaskRole, TaskState, UsageCost, UsageMetric,
    ValidationCheckResult, ValidationResult,
};

/// A persisted attempt together with the task it belongs to and execution times.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptRecord {
    task_id: TaskId,
    attempt: Attempt,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

/// A bounded historical-performance query. The interval is half-open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerformanceWindow {
    starts_at_ms: i64,
    ends_at_ms: i64,
}

impl PerformanceWindow {
    pub fn new(starts_at_ms: i64, ends_at_ms: i64) -> Result<Self, LedgerError> {
        if starts_at_ms >= ends_at_ms {
            return Err(LedgerError::InvalidPerformanceWindow);
        }
        Ok(Self {
            starts_at_ms,
            ends_at_ms,
        })
    }

    #[must_use]
    pub const fn starts_at_ms(self) -> i64 {
        self.starts_at_ms
    }
    #[must_use]
    pub const fn ends_at_ms(self) -> i64 {
        self.ends_at_ms
    }
}

/// A group keeps requested and observed targets separate. Missing observed values
/// form an explicit unknown bucket and are never attributed to the requested model.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PerformanceRequestedModel {
    Named(String),
    ProviderDefault,
    Unknown,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PerformanceTarget {
    requested_provider: String,
    requested_model: PerformanceRequestedModel,
    observed_provider: Option<String>,
    observed_model: Option<String>,
    semantics: String,
}

impl PerformanceTarget {
    #[must_use]
    pub fn requested_provider(&self) -> &str {
        &self.requested_provider
    }
    #[must_use]
    pub fn requested_model(&self) -> &PerformanceRequestedModel {
        &self.requested_model
    }
    #[must_use]
    pub fn observed_provider(&self) -> Option<&str> {
        self.observed_provider.as_deref()
    }
    #[must_use]
    pub fn observed_model(&self) -> Option<&str> {
        self.observed_model.as_deref()
    }
    #[must_use]
    pub fn semantics(&self) -> &str {
        &self.semantics
    }
}

/// Facts that can be derived from persisted Attempt and Validation rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalPerformance {
    target: PerformanceTarget,
    window: PerformanceWindow,
    attempts: u64,
    provider_call_succeeded: u64,
    provider_call_failed: u64,
    provider_call_cancelled: u64,
    provider_call_pending: u64,
    validation_passed: Option<u64>,
    validation_failed: Option<u64>,
    validation_observations: Option<u64>,
    unavailable: Vec<&'static str>,
}

/// Result envelope containing the requested interval and population coverage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoricalPerformanceQuery {
    window: PerformanceWindow,
    entries: Vec<HistoricalPerformance>,
    attempts_without_start_time: u64,
}

impl HistoricalPerformanceQuery {
    #[must_use]
    pub const fn window(&self) -> PerformanceWindow {
        self.window
    }
    #[must_use]
    pub fn entries(&self) -> &[HistoricalPerformance] {
        &self.entries
    }
    /// These records cannot be assigned to any requested interval.
    #[must_use]
    pub const fn attempts_without_start_time(&self) -> u64 {
        self.attempts_without_start_time
    }
    #[must_use]
    pub fn window_attempts(&self) -> u64 {
        self.entries
            .iter()
            .map(HistoricalPerformance::attempts)
            .sum()
    }
}

impl HistoricalPerformance {
    #[must_use]
    pub fn target(&self) -> &PerformanceTarget {
        &self.target
    }
    #[must_use]
    pub const fn window(&self) -> PerformanceWindow {
        self.window
    }
    #[must_use]
    pub const fn attempts(&self) -> u64 {
        self.attempts
    }
    #[must_use]
    pub const fn provider_call_succeeded(&self) -> u64 {
        self.provider_call_succeeded
    }
    #[must_use]
    pub const fn provider_call_failed(&self) -> u64 {
        self.provider_call_failed
    }
    #[must_use]
    pub const fn provider_call_cancelled(&self) -> u64 {
        self.provider_call_cancelled
    }
    #[must_use]
    pub const fn provider_call_pending(&self) -> u64 {
        self.provider_call_pending
    }
    #[must_use]
    pub const fn validation_passed(&self) -> Option<u64> {
        self.validation_passed
    }
    #[must_use]
    pub const fn validation_failed(&self) -> Option<u64> {
        self.validation_failed
    }
    #[must_use]
    pub const fn validation_observations(&self) -> Option<u64> {
        self.validation_observations
    }
    #[must_use]
    pub fn unavailable(&self) -> &[&'static str] {
        &self.unavailable
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PerformanceGroupKey(PerformanceTarget);

#[derive(Default)]
struct PerformanceCounts {
    attempts: u64,
    succeeded: u64,
    failed: u64,
    cancelled: u64,
    pending: u64,
    validation_passed: u64,
    validation_failed: u64,
}

impl AttemptRecord {
    #[must_use]
    pub fn new(
        task_id: TaskId,
        attempt: Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Self {
        Self {
            task_id,
            attempt,
            started_at,
            finished_at,
        }
    }

    #[must_use]
    pub const fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    #[must_use]
    pub const fn attempt(&self) -> &Attempt {
        &self.attempt
    }

    #[must_use]
    pub const fn started_at(&self) -> Option<i64> {
        self.started_at
    }

    #[must_use]
    pub const fn finished_at(&self) -> Option<i64> {
        self.finished_at
    }
}

/// Errors returned by the local execution ledger.
#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    InvalidStoredValue(String),
    InvalidPerformanceWindow,
    UnsupportedSchemaVersion(u32),
}

impl fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "execution ledger database error: {error}"),
            Self::InvalidStoredValue(value) => {
                write!(formatter, "invalid execution ledger value: {value}")
            }
            Self::InvalidPerformanceWindow => {
                formatter.write_str("performance window must have a positive duration")
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported execution ledger schema version: {version}"
                )
            }
        }
    }
}

impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::InvalidStoredValue(_)
            | Self::InvalidPerformanceWindow
            | Self::UnsupportedSchemaVersion(_) => None,
        }
    }
}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// SQLite-backed local execution ledger.
pub struct SqliteExecutionLedger {
    connection: Mutex<Connection>,
}

type PublicationRow = (
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
type PublicationTaskRow = (
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

const LATEST_SCHEMA_VERSION: u32 = 9;

/// Repository boundary for local task and attempt history.
pub trait ExecutionLedger {
    fn save_task(&self, task: &Task) -> Result<(), LedgerError>;
    fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, LedgerError>;
    fn save_attempt(
        &self,
        task_id: &TaskId,
        attempt: &Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<(), LedgerError>;
    fn get_attempt(
        &self,
        task_id: &TaskId,
        attempt_id: &AttemptId,
    ) -> Result<Option<AttemptRecord>, LedgerError>;
    fn list_attempts(&self, task_id: &TaskId) -> Result<Vec<AttemptRecord>, LedgerError>;
}

impl SqliteExecutionLedger {
    /// Opens (and initializes) a ledger at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    /// Opens an isolated in-memory ledger, useful for deterministic tests.
    pub fn open_in_memory() -> Result<Self, LedgerError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Alias for [`Self::open_in_memory`].
    pub fn in_memory() -> Result<Self, LedgerError> {
        Self::open_in_memory()
    }

    fn from_connection(connection: Connection) -> Result<Self, LedgerError> {
        // SQLite only allows changing this setting outside a transaction.  Set it
        // before starting the schema transaction so it also protects migrations.
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let transaction = connection.unchecked_transaction()?;
        let version: u32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > LATEST_SCHEMA_VERSION {
            return Err(LedgerError::UnsupportedSchemaVersion(version));
        }
        let has_tasks: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tasks')",
            [],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )?;
        if !has_tasks {
            create_latest_schema(&transaction)?;
            set_schema_version(&transaction, LATEST_SCHEMA_VERSION)?;
        } else {
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS tasks (
                 id TEXT PRIMARY KEY NOT NULL,
                 description TEXT NOT NULL,
                 role TEXT NOT NULL,
                 state TEXT NOT NULL
             );
            CREATE TABLE IF NOT EXISTS attempts (
                 task_id TEXT NOT NULL,
                 id TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 state TEXT NOT NULL,
                 started_at INTEGER,
                 finished_at INTEGER,
                 failure_reason TEXT,
                 requested_model_kind TEXT,
                 requested_model TEXT,
                 observed_provider TEXT,
                 observed_model TEXT,
                 semantics_version TEXT NOT NULL DEFAULT 'legacy_validation_coupled',
                 PRIMARY KEY (task_id, id),
                 FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS agent_results (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 summary TEXT NOT NULL,
                 reported_success INTEGER NOT NULL,
                 PRIMARY KEY (task_id, attempt_id),
                 FOREIGN KEY (task_id, attempt_id)
                     REFERENCES attempts(task_id, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS validation_results (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 summary TEXT NOT NULL,
                 passed INTEGER NOT NULL,
                 PRIMARY KEY (task_id, attempt_id, sequence),
                 FOREIGN KEY (task_id, attempt_id)
                     REFERENCES attempts(task_id, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS validation_checks (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 validation_sequence INTEGER NOT NULL,
                 sequence INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 passed INTEGER NOT NULL,
                 exit_status INTEGER,
                 diagnostics TEXT NOT NULL,
                 PRIMARY KEY (task_id, attempt_id, validation_sequence, sequence),
                 FOREIGN KEY (task_id, attempt_id, validation_sequence)
                     REFERENCES validation_results(task_id, attempt_id, sequence)
                     ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS usage_metrics (
                 task_id TEXT NOT NULL,
                 attempt_id TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 value TEXT NOT NULL,
                 unit TEXT NOT NULL,
                 PRIMARY KEY (task_id, attempt_id, sequence),
                 FOREIGN KEY (task_id, attempt_id)
                     REFERENCES attempts(task_id, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS publications (
                 idempotency_key TEXT PRIMARY KEY NOT NULL,
                 task_id TEXT,
                 repository TEXT NOT NULL,
                 branch TEXT NOT NULL,
                 commit_sha TEXT,
                 pull_request TEXT,
             phase TEXT NOT NULL,
             workspace TEXT, base TEXT, title TEXT, body TEXT
             );",
            )?;
            create_service_schema(&transaction)?;
            migrate_schema(&transaction, version)?;
        }
        create_service_schema(&transaction)?;
        transaction.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn load_publication(
        &self,
        key: &str,
    ) -> Result<Option<crate::github_workflow::PublicationRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        let row: Option<PublicationRow> = connection.query_row(
            "SELECT task_id, repository, branch, commit_sha, pull_request, phase, workspace, base, title, body FROM publications WHERE idempotency_key = ?1",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?)),
        ).optional()?;
        row.map(
            |(task_id, repository, branch, commit, pr, phase, workspace, base, title, body)| {
                crate::github_workflow::PublicationRecord::restore(
                    key.to_owned(),
                    task_id,
                    repository,
                    branch,
                    commit,
                    pr,
                    phase,
                    workspace,
                    base,
                    title,
                    body,
                )
            },
        )
        .transpose()
    }

    /// Looks up the publication associated with the conventional `issue-N` task id.
    /// Legacy records with a null task id are intentionally not inferred.
    pub fn load_publication_for_task(
        &self,
        task_id: &crate::TaskId,
    ) -> Result<Option<crate::github_workflow::PublicationRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        let row: Option<PublicationTaskRow> = connection
            .query_row(
                "SELECT idempotency_key, task_id, repository, branch, commit_sha, pull_request, phase, workspace, base, title, body
             FROM publications WHERE task_id = ?1 ORDER BY rowid DESC LIMIT 1",
                params![task_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?)),
            )
            .optional()?;
        row.map(
            |(
                key,
                task_id,
                repository,
                branch,
                commit,
                pr,
                phase,
                workspace,
                base,
                title,
                body,
            )| {
                crate::github_workflow::PublicationRecord::restore(
                    key, task_id, repository, branch, commit, pr, phase, workspace, base, title,
                    body,
                )
            },
        )
        .transpose()
    }

    pub fn save_publication(
        &self,
        record: &crate::github_workflow::PublicationRecord,
    ) -> Result<(), LedgerError> {
        let connection = self.lock_connection()?;
        connection.execute(
            "INSERT INTO publications (idempotency_key, task_id, repository, branch, commit_sha, pull_request, phase, workspace, base, title, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(idempotency_key) DO UPDATE SET repository=excluded.repository,
             task_id=COALESCE(excluded.task_id, publications.task_id), branch=excluded.branch, commit_sha=excluded.commit_sha, pull_request=excluded.pull_request, phase=excluded.phase, workspace=COALESCE(excluded.workspace, publications.workspace), base=COALESCE(excluded.base, publications.base), title=COALESCE(excluded.title, publications.title), body=COALESCE(excluded.body, publications.body)",
            params![record.idempotency_key(), record.task_id(), record.repository(), record.branch(), record.commit_sha(), record.pull_request(), record.phase().as_str(), record.workspace().map(|p| p.to_string_lossy().into_owned()), record.base(), record.title(), record.body()],
        )?;
        Ok(())
    }

    /// Saves task metadata. Existing attempt rows are retained and loaded by
    /// [`Self::get_task`], so updating a task cannot erase its history.
    pub fn save_task(&self, task: &Task) -> Result<(), LedgerError> {
        let mut connection = self.lock_connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO tasks (id, description, role, state) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET description = excluded.description,
                 role = excluded.role, state = excluded.state",
            params![
                task.id().as_str(),
                task.description(),
                task.role().as_str(),
                task_state_to_str(task.state()),
            ],
        )?;
        transaction.execute(
            "INSERT INTO service_task_revisions(task_id, revision) VALUES (?1, 0)
             ON CONFLICT(task_id) DO UPDATE SET revision=revision+1",
            params![task.id().as_str()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Retrieves a task and all of its attempts, ordered by insertion id.
    pub fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, LedgerError> {
        let connection = self.lock_connection()?;
        self.get_task_with_connection(&connection, task_id)
    }

    pub(crate) fn get_task_with_connection(
        &self,
        connection: &Connection,
        task_id: &TaskId,
    ) -> Result<Option<Task>, LedgerError> {
        let task = connection
            .query_row(
                "SELECT description, role, state FROM tasks WHERE id = ?1",
                params![task_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((description, role, state)) = task else {
            return Ok(None);
        };
        let attempts = self.load_attempts(connection, task_id)?;
        Ok(Some(Task::restore(
            task_id.clone(),
            description,
            TaskRole::new(role),
            task_state_from_str(&state)?,
            attempts.into_iter().map(|record| record.attempt).collect(),
        )))
    }

    /// Inserts or updates one attempt without affecting any other attempt.
    pub fn save_attempt(
        &self,
        task_id: &TaskId,
        attempt: &Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<(), LedgerError> {
        let record = AttemptRecord::new(task_id.clone(), attempt.clone(), started_at, finished_at);
        self.save_attempt_record(&record)
    }

    /// Inserts or updates one attempt record.
    pub fn save_attempt_record(&self, record: &AttemptRecord) -> Result<(), LedgerError> {
        let connection = self.lock_connection()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO attempts (task_id, id, provider, state, started_at, finished_at, failure_reason,
                 requested_model_kind, requested_model, observed_provider, observed_model, semantics_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(task_id, id) DO UPDATE SET provider = excluded.provider,
                 state = excluded.state, started_at = excluded.started_at,
                 finished_at = excluded.finished_at, failure_reason = excluded.failure_reason,
                 requested_model_kind = excluded.requested_model_kind,
                 requested_model = excluded.requested_model,
                 observed_provider = excluded.observed_provider,
                 observed_model = excluded.observed_model,
                 semantics_version = excluded.semantics_version",
            params![
                record.task_id().as_str(),
                record.attempt().id().as_str(),
                record.attempt().provider().as_str(),
                attempt_state_to_str(record.attempt().state()),
                record.started_at(),
                record.finished_at(),
                record.attempt().failure_reason().map(failure_reason_to_str),
                record.attempt().requested_model().map(model_choice_kind),
                record.attempt().requested_model().and_then(model_choice_name),
                record.attempt().observed_provider().map(ProviderRef::as_str),
                record.attempt().observed_model().map(ModelRef::as_str),
                record.attempt().semantics().as_str(),
            ],
        )?;
        transaction.execute(
            "DELETE FROM agent_results WHERE task_id = ?1 AND attempt_id = ?2",
            params![record.task_id().as_str(), record.attempt().id().as_str()],
        )?;
        if let Some(result) = record.attempt().agent_result() {
            transaction.execute(
                "INSERT INTO agent_results
                 (task_id, attempt_id, summary, reported_success) VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.task_id().as_str(),
                    record.attempt().id().as_str(),
                    result.summary(),
                    bool_to_int(result.reported_success()),
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM validation_results WHERE task_id = ?1 AND attempt_id = ?2",
            params![record.task_id().as_str(), record.attempt().id().as_str()],
        )?;
        for (sequence, result) in record.attempt().validation_results().iter().enumerate() {
            let sequence = i64::try_from(sequence).map_err(|_| {
                LedgerError::InvalidStoredValue("validation sequence overflow".into())
            })?;
            transaction.execute(
                "INSERT INTO validation_results
                 (task_id, attempt_id, sequence, summary, passed) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.task_id().as_str(),
                    record.attempt().id().as_str(),
                    sequence,
                    result.summary(),
                    bool_to_int(result.passed()),
                ],
            )?;
            for (check_sequence, check) in result.checks().iter().enumerate() {
                let check_sequence = i64::try_from(check_sequence).map_err(|_| {
                    LedgerError::InvalidStoredValue("validation check sequence overflow".into())
                })?;
                transaction.execute(
                    "INSERT INTO validation_checks
                     (task_id, attempt_id, validation_sequence, sequence, name, passed,
                      exit_status, diagnostics)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        record.task_id().as_str(),
                        record.attempt().id().as_str(),
                        sequence,
                        check_sequence,
                        check.name(),
                        bool_to_int(check.passed()),
                        check.exit_status(),
                        check.diagnostics(),
                    ],
                )?;
            }
        }
        transaction.execute(
            "DELETE FROM usage_metrics WHERE task_id = ?1 AND attempt_id = ?2",
            params![record.task_id().as_str(), record.attempt().id().as_str()],
        )?;
        if let Some(usage) = record.attempt().usage_cost() {
            for (sequence, metric) in usage.metrics().iter().enumerate() {
                transaction.execute(
                    "INSERT INTO usage_metrics
                     (task_id, attempt_id, sequence, name, value, unit)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        record.task_id().as_str(),
                        record.attempt().id().as_str(),
                        i64::try_from(sequence).map_err(|_| {
                            LedgerError::InvalidStoredValue("usage sequence overflow".into())
                        })?,
                        metric.name(),
                        metric.value(),
                        metric.unit(),
                    ],
                )?;
            }
        }
        let revised = transaction.execute(
            "UPDATE service_task_revisions SET revision=revision+1 WHERE task_id=?1",
            params![record.task_id().as_str()],
        )?;
        if revised != 1 {
            return Err(LedgerError::InvalidStoredValue(
                "Task revision row is missing".into(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    /// Retrieves one attempt for a task.
    pub fn get_attempt(
        &self,
        task_id: &TaskId,
        attempt_id: &AttemptId,
    ) -> Result<Option<AttemptRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        let record = connection
            .query_row(
                "SELECT provider, state, started_at, finished_at, failure_reason,
                        requested_model_kind, requested_model, observed_provider, observed_model,
                        semantics_version
                 FROM attempts WHERE task_id = ?1 AND id = ?2",
                params![task_id.as_str(), attempt_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            provider,
            state,
            started_at,
            finished_at,
            reason,
            model_kind,
            model_name,
            observed_provider,
            observed_model,
            semantics_version,
        )) = record
        else {
            return Ok(None);
        };
        let attempt = self.load_attempt_details(
            &connection,
            task_id,
            attempt_id,
            ProviderRef::new(provider),
            AttemptTargets {
                requested_model: requested_model_from_storage(
                    model_kind.as_deref(),
                    model_name.as_deref(),
                )?,
                observed_provider: observed_provider.map(ProviderRef::new),
                observed_model: observed_model.map(ModelRef::new),
            },
            attempt_state_from_str(&state)?,
            reason.map(|r| failure_reason_from_str(&r)).transpose()?,
            AttemptSemantics::from_str(&semantics_version)
                .map_err(LedgerError::InvalidStoredValue)?,
        )?;
        Ok(Some(AttemptRecord::new(
            task_id.clone(),
            attempt,
            started_at,
            finished_at,
        )))
    }

    /// Retrieves every attempt for a task in insertion order.
    pub fn list_attempts(&self, task_id: &TaskId) -> Result<Vec<AttemptRecord>, LedgerError> {
        let connection = self.lock_connection()?;
        self.load_attempts(&connection, task_id)
    }

    /// Aggregates only persisted Attempt and Validation facts in the requested
    /// half-open time window. Rows without a known start time cannot be assigned
    /// to a window and are therefore excluded. Unsupported facts remain explicit.
    pub fn query_historical_performance(
        &self,
        window: PerformanceWindow,
    ) -> Result<HistoricalPerformanceQuery, LedgerError> {
        let connection = self.lock_connection()?;
        let attempts_without_start_time = connection.query_row(
            "SELECT COUNT(*) FROM attempts WHERE started_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT a.provider, a.requested_model_kind, a.requested_model,
                    a.observed_provider, a.observed_model, a.semantics_version, a.state,
                    (SELECT COUNT(*) FROM validation_results v
                     WHERE v.task_id=a.task_id AND v.attempt_id=a.id),
                    (SELECT COALESCE(SUM(v.passed),0) FROM validation_results v
                     WHERE v.task_id=a.task_id AND v.attempt_id=a.id)
             FROM attempts a
             WHERE a.started_at >= ?1 AND a.started_at < ?2
             ORDER BY a.rowid",
        )?;
        let rows =
            statement.query_map(params![window.starts_at_ms(), window.ends_at_ms()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, u64>(7)?,
                    row.get::<_, u64>(8)?,
                ))
            })?;
        let mut groups = BTreeMap::<PerformanceGroupKey, PerformanceCounts>::new();
        for row in rows {
            let (
                requested_provider,
                requested_model_kind,
                requested_model_name,
                observed_provider,
                observed_model,
                semantics,
                state,
                validation_observations,
                validation_passed,
            ) = row?;
            let requested_model = match requested_model_kind.as_deref() {
                Some("named") => requested_model_name
                    .map(PerformanceRequestedModel::Named)
                    .ok_or_else(|| {
                        LedgerError::InvalidStoredValue("named requested model has no name".into())
                    })?,
                Some("provider_default") if requested_model_name.is_none() => {
                    PerformanceRequestedModel::ProviderDefault
                }
                None if requested_model_name.is_none() => PerformanceRequestedModel::Unknown,
                Some(other) => {
                    return Err(LedgerError::InvalidStoredValue(format!(
                        "unknown requested model kind in history: {other}"
                    )));
                }
                _ => {
                    return Err(LedgerError::InvalidStoredValue(
                        "invalid requested model fields in history".into(),
                    ));
                }
            };
            let key = PerformanceGroupKey(PerformanceTarget {
                requested_provider,
                requested_model,
                observed_provider,
                observed_model,
                semantics: semantics.clone(),
            });
            if semantics != "provider_call_v2" && semantics != "legacy_validation_coupled" {
                return Err(LedgerError::InvalidStoredValue(format!(
                    "unknown Attempt semantics in history: {semantics}"
                )));
            }
            let counts = groups.entry(key).or_default();
            counts.attempts += 1;
            if semantics == "provider_call_v2" {
                match state.as_str() {
                    "succeeded" => counts.succeeded += 1,
                    "failed" => counts.failed += 1,
                    "cancelled" => counts.cancelled += 1,
                    "queued" | "running" => counts.pending += 1,
                    other => {
                        return Err(LedgerError::InvalidStoredValue(format!(
                            "unknown Attempt state in history: {other}"
                        )));
                    }
                }
            }
            if semantics == "legacy_validation_coupled" {
                counts.validation_passed += validation_passed;
                counts.validation_failed += validation_observations - validation_passed;
            }
        }
        let entries = groups
            .into_iter()
            .map(|(PerformanceGroupKey(target), counts)| {
                let mut unavailable = vec![
                    "retry/rework relations are not stored in this Ledger",
                    "review verdicts are not stored in this Ledger",
                    "Codex acceptance is not stored with a completion timestamp",
                ];
                if target.semantics == "legacy_validation_coupled" {
                    unavailable.push(
                        "legacy Attempt terminal states do not represent Provider call outcomes",
                    );
                } else {
                    unavailable.push(
                        "V2 Validation is Artifact-scoped but the Ledger stores no Artifact identity for it",
                    );
                }
                let validation_available = target.semantics == "legacy_validation_coupled";
                HistoricalPerformance {
                    target,
                    window,
                    attempts: counts.attempts,
                    provider_call_succeeded: counts.succeeded,
                    provider_call_failed: counts.failed,
                    provider_call_cancelled: counts.cancelled,
                    provider_call_pending: counts.pending,
                    validation_passed: validation_available.then_some(counts.validation_passed),
                    validation_failed: validation_available.then_some(counts.validation_failed),
                    validation_observations: validation_available
                        .then_some(counts.validation_passed + counts.validation_failed),
                    unavailable,
                }
            })
            .collect();
        Ok(HistoricalPerformanceQuery {
            window,
            entries,
            attempts_without_start_time,
        })
    }

    pub(crate) fn load_attempts(
        &self,
        connection: &Connection,
        task_id: &TaskId,
    ) -> Result<Vec<AttemptRecord>, LedgerError> {
        let mut statement = connection.prepare(
            "SELECT id, provider, state, started_at, finished_at, failure_reason,
                    requested_model_kind, requested_model, observed_provider, observed_model,
                    semantics_version
             FROM attempts WHERE task_id = ?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![task_id.as_str()], |row| {
            Ok((
                AttemptId::new(row.get::<_, String>(0)?),
                ProviderRef::new(row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, String>(10)?,
            ))
        })?;
        let mut attempts = Vec::new();
        for row in rows {
            let (
                attempt_id,
                provider,
                state,
                started_at,
                finished_at,
                reason,
                model_kind,
                model_name,
                observed_provider,
                observed_model,
                semantics_version,
            ) = row?;
            let attempt = self.load_attempt_details(
                connection,
                task_id,
                &attempt_id,
                provider,
                AttemptTargets {
                    requested_model: requested_model_from_storage(
                        model_kind.as_deref(),
                        model_name.as_deref(),
                    )?,
                    observed_provider: observed_provider.map(ProviderRef::new),
                    observed_model: observed_model.map(ModelRef::new),
                },
                attempt_state_from_str(&state)?,
                reason.map(|r| failure_reason_from_str(&r)).transpose()?,
                AttemptSemantics::from_str(&semantics_version)
                    .map_err(LedgerError::InvalidStoredValue)?,
            )?;
            attempts.push(AttemptRecord::new(
                task_id.clone(),
                attempt,
                started_at,
                finished_at,
            ));
        }
        Ok(attempts)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn load_attempt_details(
        &self,
        connection: &Connection,
        task_id: &TaskId,
        attempt_id: &AttemptId,
        provider: ProviderRef,
        targets: AttemptTargets,
        state: AttemptState,
        failure_reason: Option<AttemptFailureReason>,
        semantics: AttemptSemantics,
    ) -> Result<Attempt, LedgerError> {
        let agent_result = connection
            .query_row(
                "SELECT summary, reported_success FROM agent_results
                 WHERE task_id = ?1 AND attempt_id = ?2",
                params![task_id.as_str(), attempt_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let agent_result = agent_result
            .map(|(summary, reported_success)| {
                int_to_bool(reported_success)
                    .map(|reported_success| AgentResult::new(summary, reported_success))
            })
            .transpose()?;
        let aggregate_rows = {
            let mut statement = connection.prepare(
                "SELECT sequence, summary, passed FROM validation_results
                 WHERE task_id = ?1 AND attempt_id = ?2 ORDER BY sequence",
            )?;
            let rows =
                statement.query_map(params![task_id.as_str(), attempt_id.as_str()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut validations = Vec::with_capacity(aggregate_rows.len());
        for (sequence, summary, passed) in aggregate_rows {
            let mut statement = connection.prepare(
                "SELECT name, passed, exit_status, diagnostics
                 FROM validation_checks
                 WHERE task_id = ?1 AND attempt_id = ?2 AND validation_sequence = ?3
                 ORDER BY sequence",
            )?;
            let rows = statement.query_map(
                params![task_id.as_str(), attempt_id.as_str(), sequence],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i32>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )?;
            let mut checks = Vec::new();
            for row in rows {
                let (name, check_passed, exit_status, diagnostics) = row?;
                checks.push(ValidationCheckResult::new(
                    name,
                    int_to_bool(check_passed)?,
                    exit_status,
                    diagnostics,
                ));
            }
            validations.push(ValidationResult::restore(
                summary,
                int_to_bool(passed)?,
                checks,
            ));
        }
        let mut metrics = Vec::new();
        let mut statement = connection.prepare(
            "SELECT name, value, unit FROM usage_metrics
             WHERE task_id = ?1 AND attempt_id = ?2 ORDER BY sequence",
        )?;
        let rows = statement.query_map(params![task_id.as_str(), attempt_id.as_str()], |row| {
            Ok(UsageMetric::new(
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            metrics.push(row?);
        }
        let usage_cost = (!metrics.is_empty()).then(|| UsageCost::new(metrics));
        Ok(Attempt::restore(
            attempt_id.clone(),
            provider,
            targets,
            state,
            agent_result,
            validations,
            usage_cost,
            failure_reason,
            semantics,
        ))
    }

    pub(crate) fn lock_connection(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Connection>, LedgerError> {
        self.connection.lock().map_err(|_| {
            LedgerError::InvalidStoredValue("execution ledger mutex was poisoned".into())
        })
    }
}

fn set_schema_version(connection: &Connection, version: u32) -> Result<(), rusqlite::Error> {
    connection.execute_batch(&format!("PRAGMA user_version = {version};"))
}

fn create_latest_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "CREATE TABLE tasks (id TEXT PRIMARY KEY NOT NULL, description TEXT NOT NULL, role TEXT NOT NULL, state TEXT NOT NULL);
         CREATE TABLE attempts (task_id TEXT NOT NULL, id TEXT NOT NULL, provider TEXT NOT NULL, state TEXT NOT NULL,
             started_at INTEGER, finished_at INTEGER, failure_reason TEXT, requested_model_kind TEXT,
             requested_model TEXT, observed_provider TEXT, observed_model TEXT,
             semantics_version TEXT NOT NULL DEFAULT 'legacy_validation_coupled', PRIMARY KEY (task_id, id),
             FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE);
         CREATE TABLE agent_results (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, summary TEXT NOT NULL,
             reported_success INTEGER NOT NULL, PRIMARY KEY (task_id, attempt_id),
             FOREIGN KEY (task_id, attempt_id) REFERENCES attempts(task_id, id) ON DELETE CASCADE);
         CREATE TABLE validation_results (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, sequence INTEGER NOT NULL,
             summary TEXT NOT NULL, passed INTEGER NOT NULL, PRIMARY KEY (task_id, attempt_id, sequence),
             FOREIGN KEY (task_id, attempt_id) REFERENCES attempts(task_id, id) ON DELETE CASCADE);
         CREATE TABLE validation_checks (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, validation_sequence INTEGER NOT NULL,
             sequence INTEGER NOT NULL, name TEXT NOT NULL, passed INTEGER NOT NULL, exit_status INTEGER, diagnostics TEXT NOT NULL,
             PRIMARY KEY (task_id, attempt_id, validation_sequence, sequence),
             FOREIGN KEY (task_id, attempt_id, validation_sequence)
                 REFERENCES validation_results(task_id, attempt_id, sequence) ON DELETE CASCADE);
         CREATE TABLE usage_metrics (task_id TEXT NOT NULL, attempt_id TEXT NOT NULL, sequence INTEGER NOT NULL,
             name TEXT NOT NULL, value TEXT NOT NULL, unit TEXT NOT NULL, PRIMARY KEY (task_id, attempt_id, sequence),
             FOREIGN KEY (task_id, attempt_id) REFERENCES attempts(task_id, id) ON DELETE CASCADE);
         CREATE TABLE publications (idempotency_key TEXT PRIMARY KEY NOT NULL, task_id TEXT, repository TEXT NOT NULL,
             branch TEXT NOT NULL, commit_sha TEXT, pull_request TEXT, phase TEXT NOT NULL,
             workspace TEXT, base TEXT, title TEXT, body TEXT);",
    )
}

fn column_exists(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), rusqlite::Error> {
    if !column_exists(connection, table, column)? {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn migrate_schema(connection: &Connection, version: u32) -> Result<(), LedgerError> {
    // Each step is idempotent so version-zero databases from every historical
    // generation can be upgraded while retaining all existing row values.
    for target in (version + 1)..=LATEST_SCHEMA_VERSION {
        match target {
            1 => add_column_if_missing(connection, "attempts", "failure_reason", "TEXT")?,
            2 => add_column_if_missing(connection, "publications", "task_id", "TEXT")?,
            3 => add_column_if_missing(connection, "publications", "workspace", "TEXT")?,
            4 => add_column_if_missing(connection, "publications", "base", "TEXT")?,
            5 => add_column_if_missing(connection, "publications", "title", "TEXT")?,
            6 => add_column_if_missing(connection, "publications", "body", "TEXT")?,
            7 => {
                add_column_if_missing(connection, "attempts", "requested_model_kind", "TEXT")?;
                add_column_if_missing(connection, "attempts", "requested_model", "TEXT")?;
                add_column_if_missing(connection, "attempts", "observed_provider", "TEXT")?;
                add_column_if_missing(connection, "attempts", "observed_model", "TEXT")?;
            }
            8 => add_column_if_missing(
                connection,
                "attempts",
                "semantics_version",
                "TEXT NOT NULL DEFAULT 'legacy_validation_coupled'",
            )?,
            9 => {
                add_column_if_missing(connection, "service_operations", "workspace_path", "TEXT")?;
                add_column_if_missing(
                    connection,
                    "service_operations",
                    "workspace_branch",
                    "TEXT",
                )?;
            }
            _ => unreachable!(),
        }
        set_schema_version(connection, target)?;
    }
    Ok(())
}

fn create_service_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS service_task_revisions (
             task_id TEXT PRIMARY KEY NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
             revision INTEGER NOT NULL CHECK(revision >= 0)
         );
         CREATE TABLE IF NOT EXISTS service_operations (
             id TEXT PRIMARY KEY NOT NULL,
             request_id TEXT NOT NULL UNIQUE,
             task_id TEXT NOT NULL REFERENCES tasks(id),
             attempt_id TEXT NOT NULL,
             expected_revision INTEGER NOT NULL,
             provider TEXT NOT NULL,
             model_kind TEXT NOT NULL,
             model_name TEXT,
             instruction TEXT NOT NULL,
             role TEXT NOT NULL,
             repository TEXT NOT NULL,
             base_commit TEXT NOT NULL,
             timeout_override_ms INTEGER,
             timeout_ms INTEGER NOT NULL,
             status TEXT NOT NULL,
             accepted_at INTEGER NOT NULL,
             started_at INTEGER,
             finished_at INTEGER,
             observed_provider TEXT,
             observed_model TEXT,
             workspace_path TEXT,
             workspace_branch TEXT,
             diagnostic_code TEXT,
             FOREIGN KEY(task_id, attempt_id) REFERENCES attempts(task_id, id)
         );
         CREATE TABLE IF NOT EXISTS service_operation_usage (
             operation_id TEXT NOT NULL REFERENCES service_operations(id) ON DELETE CASCADE,
             sequence INTEGER NOT NULL,
             name TEXT NOT NULL,
             value TEXT NOT NULL,
             unit TEXT NOT NULL,
             PRIMARY KEY(operation_id, sequence)
         );
         CREATE INDEX IF NOT EXISTS service_operations_task_status
             ON service_operations(task_id, status);
         INSERT OR IGNORE INTO service_task_revisions(task_id, revision)
             SELECT id, 0 FROM tasks;",
    )
}

pub(crate) fn model_choice_kind(choice: &ModelChoice) -> &'static str {
    match choice {
        ModelChoice::Named(_) => "named",
        ModelChoice::ProviderDefault => "provider_default",
    }
}

pub(crate) fn model_choice_name(choice: &ModelChoice) -> Option<&str> {
    match choice {
        ModelChoice::Named(model) => Some(model.as_str()),
        ModelChoice::ProviderDefault => None,
    }
}

pub(crate) fn requested_model_from_storage(
    kind: Option<&str>,
    name: Option<&str>,
) -> Result<Option<ModelChoice>, LedgerError> {
    match (kind, name) {
        (None, None) => Ok(None), // Legacy attempt: requested model was not recorded.
        (Some("named"), Some(name)) => Ok(Some(ModelChoice::Named(ModelRef::new(name)))),
        (Some("provider_default"), None) => Ok(Some(ModelChoice::ProviderDefault)),
        _ => Err(LedgerError::InvalidStoredValue(format!(
            "invalid requested model fields: kind={kind:?}, name={name:?}"
        ))),
    }
}

impl ExecutionLedger for SqliteExecutionLedger {
    fn save_task(&self, task: &Task) -> Result<(), LedgerError> {
        Self::save_task(self, task)
    }

    fn get_task(&self, task_id: &TaskId) -> Result<Option<Task>, LedgerError> {
        Self::get_task(self, task_id)
    }

    fn save_attempt(
        &self,
        task_id: &TaskId,
        attempt: &Attempt,
        started_at: Option<i64>,
        finished_at: Option<i64>,
    ) -> Result<(), LedgerError> {
        Self::save_attempt(self, task_id, attempt, started_at, finished_at)
    }

    fn get_attempt(
        &self,
        task_id: &TaskId,
        attempt_id: &AttemptId,
    ) -> Result<Option<AttemptRecord>, LedgerError> {
        Self::get_attempt(self, task_id, attempt_id)
    }

    fn list_attempts(&self, task_id: &TaskId) -> Result<Vec<AttemptRecord>, LedgerError> {
        Self::list_attempts(self, task_id)
    }
}

fn bool_to_int(value: bool) -> i64 {
    i64::from(value)
}

fn int_to_bool(value: i64) -> Result<bool, LedgerError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(LedgerError::InvalidStoredValue(format!(
            "boolean value {other}"
        ))),
    }
}

pub(crate) fn task_state_to_str(state: TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Active => "active",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}

fn task_state_from_str(value: &str) -> Result<TaskState, LedgerError> {
    match value {
        "pending" => Ok(TaskState::Pending),
        "active" => Ok(TaskState::Active),
        "completed" => Ok(TaskState::Completed),
        "failed" => Ok(TaskState::Failed),
        "cancelled" => Ok(TaskState::Cancelled),
        other => Err(LedgerError::InvalidStoredValue(format!(
            "task state {other}"
        ))),
    }
}

pub(crate) fn attempt_state_to_str(state: AttemptState) -> &'static str {
    match state {
        AttemptState::Queued => "queued",
        AttemptState::Running => "running",
        AttemptState::Validating => "validating",
        AttemptState::Succeeded => "succeeded",
        AttemptState::Failed => "failed",
        AttemptState::Cancelled => "cancelled",
    }
}

fn attempt_state_from_str(value: &str) -> Result<AttemptState, LedgerError> {
    match value {
        "queued" => Ok(AttemptState::Queued),
        "running" => Ok(AttemptState::Running),
        "validating" => Ok(AttemptState::Validating),
        "succeeded" => Ok(AttemptState::Succeeded),
        "failed" => Ok(AttemptState::Failed),
        "cancelled" => Ok(AttemptState::Cancelled),
        other => Err(LedgerError::InvalidStoredValue(format!(
            "attempt state {other}"
        ))),
    }
}

pub(crate) fn failure_reason_to_str(reason: &AttemptFailureReason) -> &'static str {
    match reason {
        AttemptFailureReason::Provider => "provider",
        AttemptFailureReason::Timeout => "timeout",
        AttemptFailureReason::Cancelled => "cancelled",
        AttemptFailureReason::Workspace => "workspace",
        AttemptFailureReason::Validation => "validation",
    }
}
fn failure_reason_from_str(value: &str) -> Result<AttemptFailureReason, LedgerError> {
    match value {
        "provider" => Ok(AttemptFailureReason::Provider),
        "timeout" => Ok(AttemptFailureReason::Timeout),
        "cancelled" => Ok(AttemptFailureReason::Cancelled),
        "workspace" => Ok(AttemptFailureReason::Workspace),
        "validation" => Ok(AttemptFailureReason::Validation),
        _ => Err(LedgerError::InvalidStoredValue(value.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> Task {
        Task::new(
            TaskId::new("task-1"),
            "implement ledger",
            TaskRole::new("developer"),
        )
    }

    fn attempt(id: &str, provider: &str) -> Attempt {
        Attempt::new(
            AttemptId::new(id),
            ProviderRef::new(provider),
            ModelChoice::ProviderDefault,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn history_fixture_attempt(
        ledger: &SqliteExecutionLedger,
        id: &str,
        state: &str,
        started_at: Option<i64>,
        requested_model_kind: Option<&str>,
        requested_model: Option<&str>,
        observed_provider: Option<&str>,
        observed_model: Option<&str>,
        semantics: &str,
        validations: &[(i64, i64)],
    ) {
        ledger
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO attempts(task_id,id,provider,state,started_at,requested_model_kind,
                requested_model,observed_provider,observed_model,semantics_version)
             VALUES('task-1',?1,'codex',?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    state,
                    started_at,
                    requested_model_kind,
                    requested_model,
                    observed_provider,
                    observed_model,
                    semantics
                ],
            )
            .unwrap();
        for (sequence, passed) in validations {
            ledger
                .connection
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO validation_results(task_id,attempt_id,sequence,summary,passed)
                 VALUES('task-1',?1,?2,'fixture',?3)",
                    params![id, sequence, passed],
                )
                .unwrap();
        }
    }

    #[test]
    fn history_query_uses_observed_target_and_respects_window_and_semantics() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger.save_task(&task()).unwrap();
        history_fixture_attempt(
            &ledger,
            "known",
            "succeeded",
            Some(100),
            Some("named"),
            Some("requested-a"),
            Some("codex"),
            Some("observed-a"),
            "provider_call_v2",
            &[(0, 1)],
        );
        history_fixture_attempt(
            &ledger,
            "unknown",
            "failed",
            Some(200),
            Some("named"),
            Some("requested-a"),
            Some("codex"),
            None,
            "provider_call_v2",
            &[],
        );
        history_fixture_attempt(
            &ledger,
            "other",
            "cancelled",
            Some(300),
            Some("named"),
            Some("requested-a"),
            Some("codex"),
            Some("observed-b"),
            "provider_call_v2",
            &[(0, 0)],
        );
        history_fixture_attempt(
            &ledger,
            "legacy",
            "succeeded",
            Some(400),
            Some("named"),
            Some("requested-a"),
            None,
            None,
            "legacy_validation_coupled",
            &[(0, 1)],
        );
        history_fixture_attempt(
            &ledger,
            "untimed",
            "failed",
            None,
            Some("named"),
            Some("requested-a"),
            None,
            None,
            "provider_call_v2",
            &[],
        );

        let results = ledger
            .query_historical_performance(PerformanceWindow::new(100, 400).unwrap())
            .unwrap();
        assert_eq!(results.entries().len(), 3);
        assert_eq!(results.window_attempts(), 3);
        assert_eq!(results.attempts_without_start_time(), 1);
        let observed = results
            .entries()
            .iter()
            .find(|entry| entry.target().observed_model() == Some("observed-a"))
            .unwrap();
        assert_eq!(observed.attempts(), 1);
        assert_eq!(observed.provider_call_succeeded(), 1);
        assert_eq!(observed.validation_observations(), None);
        assert_eq!(observed.validation_passed(), None);
        assert_eq!(observed.validation_failed(), None);
        assert!(
            observed
                .unavailable()
                .iter()
                .any(|reason| { reason.contains("V2 Validation is Artifact-scoped") })
        );
        assert_eq!(
            observed.target().requested_model(),
            &PerformanceRequestedModel::Named("requested-a".into())
        );

        let unknown = results
            .entries()
            .iter()
            .find(|entry| entry.target().observed_model().is_none())
            .unwrap();
        assert_eq!(
            unknown.target().requested_model(),
            &PerformanceRequestedModel::Named("requested-a".into())
        );
        assert_eq!(unknown.provider_call_failed(), 1);
        assert!(results.entries().iter().all(|entry| {
            entry
                .unavailable()
                .iter()
                .any(|reason| reason.contains("review verdicts"))
        }));

        let legacy = ledger
            .query_historical_performance(PerformanceWindow::new(400, 401).unwrap())
            .unwrap();
        assert_eq!(legacy.window_attempts(), 1);
        let legacy_entry = &legacy.entries()[0];
        assert_eq!(legacy_entry.provider_call_succeeded(), 0);
        assert!(
            legacy_entry
                .unavailable()
                .iter()
                .any(|reason| reason.contains("legacy Attempt terminal states"))
        );
        assert_eq!(legacy_entry.validation_observations(), Some(1));
        assert_eq!(legacy_entry.validation_passed(), Some(1));
    }

    #[test]
    fn history_query_rejects_empty_or_reversed_window() {
        assert!(matches!(
            PerformanceWindow::new(10, 10),
            Err(LedgerError::InvalidPerformanceWindow)
        ));
        assert!(matches!(
            PerformanceWindow::new(11, 10),
            Err(LedgerError::InvalidPerformanceWindow)
        ));
    }

    #[test]
    fn schema_is_initialized_and_task_round_trips() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        assert_eq!(ledger.get_task(task.id()).unwrap(), Some(task));
    }

    #[test]
    fn foreign_key_enforcement_is_enabled_before_schema_transaction() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let connection = ledger.connection.lock().unwrap();
        let enabled: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(enabled, 1);
        let error = connection
            .execute(
                "INSERT INTO attempts (task_id, id, provider, state) VALUES ('missing', 'a', 'codex', 'queued')",
                [],
            )
            .unwrap_err();
        assert!(matches!(error, rusqlite::Error::SqliteFailure(_, _)));
    }

    #[test]
    fn invalid_publication_phase_is_rejected_when_restored() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO publications (idempotency_key, repository, branch, phase) VALUES ('bad', 'o/r', 'main', 'future')",
                [],
            )
            .unwrap();
        assert!(matches!(
            ledger.load_publication("bad"),
            Err(LedgerError::InvalidStoredValue(_))
        ));
    }

    #[test]
    fn published_publication_without_pull_request_is_rejected_when_restored() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        ledger
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO publications (idempotency_key, repository, branch, phase) VALUES ('missing-pr', 'o/r', 'main', 'published')",
                [],
            )
            .unwrap();
        assert!(matches!(
            ledger.load_publication("missing-pr"),
            Err(LedgerError::InvalidStoredValue(_))
        ));
    }

    #[test]
    fn attempt_round_trips_results_state_and_timestamps() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let mut attempt = attempt("attempt-1", "codex");
        attempt.start().unwrap();
        attempt
            .record_agent_result(AgentResult::new("done", true))
            .unwrap();
        attempt.finish().unwrap();
        attempt
            .apply_validation(ValidationResult::new("tests passed", true))
            .unwrap();
        attempt.set_usage_cost(UsageCost::new([UsageMetric::new("tokens", "42", "count")]));
        ledger
            .save_attempt(task.id(), &attempt, Some(1_000), Some(2_000))
            .unwrap();
        let record = ledger
            .get_attempt(task.id(), attempt.id())
            .unwrap()
            .unwrap();
        assert_eq!(record.attempt(), &attempt);
        assert_eq!(record.started_at(), Some(1_000));
        assert_eq!(record.finished_at(), Some(2_000));
    }

    #[test]
    fn validation_round_trips_aggregate_and_all_check_details() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let mut attempt = attempt("attempt-1", "codex");
        attempt.start().unwrap();
        attempt.finish().unwrap();
        let validation = ValidationResult::from_checks(
            "one check failed",
            [
                ValidationCheckResult::new("cargo test", true, Some(0), ""),
                ValidationCheckResult::new(
                    "cargo clippy",
                    false,
                    None,
                    "warning: diagnostic details",
                ),
            ],
        );
        attempt.apply_validation(validation.clone()).unwrap();
        ledger
            .save_attempt(task.id(), &attempt, Some(10), Some(20))
            .unwrap();

        let loaded = ledger
            .get_attempt(task.id(), attempt.id())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.attempt().validation_results(), &[validation]);
        assert_eq!(loaded.attempt().state(), AttemptState::Failed);
    }

    #[test]
    fn retry_adds_history_without_overwriting_previous_attempt() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let first = attempt("attempt-1", "codex");
        let second = attempt("attempt-2", "antigravity");
        ledger
            .save_attempt(task.id(), &first, Some(10), Some(20))
            .unwrap();
        ledger
            .save_attempt(task.id(), &second, Some(30), None)
            .unwrap();
        let attempts = ledger.list_attempts(task.id()).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].attempt().provider().as_str(), "codex");
        assert_eq!(attempts[1].attempt().provider().as_str(), "antigravity");
        assert_eq!(attempts[0].finished_at(), Some(20));
    }

    #[test]
    fn task_load_includes_linked_attempts() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let attempt = attempt("attempt-1", "codex");
        ledger
            .save_attempt(task.id(), &attempt, None, None)
            .unwrap();
        let loaded = ledger.get_task(task.id()).unwrap().unwrap();
        assert_eq!(loaded.attempts(), &[attempt]);
    }

    #[test]
    fn attempt_requested_and_observed_targets_round_trip() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        let task = task();
        ledger.save_task(&task).unwrap();
        let mut attempt = Attempt::new(
            AttemptId::new("attempt-model"),
            ProviderRef::new("codex"),
            ModelChoice::Named(ModelRef::new("gpt-test")),
        );
        attempt.record_observed_target(
            Some(ProviderRef::new("codex")),
            Some(ModelRef::new("gpt-test")),
        );
        ledger
            .save_attempt(task.id(), &attempt, Some(1), Some(2))
            .unwrap();
        let loaded = ledger
            .get_attempt(task.id(), attempt.id())
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.attempt().requested_model(),
            attempt.requested_model()
        );
        assert_eq!(
            loaded.attempt().observed_provider(),
            attempt.observed_provider()
        );
        assert_eq!(loaded.attempt().observed_model(), attempt.observed_model());
    }

    #[test]
    fn file_ledger_survives_connection_reopen() {
        let path = std::env::temp_dir().join(format!(
            "ai-dev-orchestrator-ledger-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let task = task();
        let attempt = attempt("attempt-1", "codex");
        {
            let ledger = SqliteExecutionLedger::open(&path).unwrap();
            ledger.save_task(&task).unwrap();
            ledger
                .save_attempt(task.id(), &attempt, Some(100), Some(200))
                .unwrap();
        }
        let reopened = SqliteExecutionLedger::open(&path).unwrap();
        let record = reopened
            .get_attempt(task.id(), attempt.id())
            .unwrap()
            .unwrap();
        assert_eq!(record.attempt(), &attempt);
        assert_eq!(record.started_at(), Some(100));
        assert_eq!(record.finished_at(), Some(200));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_schema_migration_preserves_attempt_and_publication_data() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY NOT NULL, description TEXT NOT NULL, role TEXT NOT NULL, state TEXT NOT NULL);
                 CREATE TABLE attempts (task_id TEXT NOT NULL, id TEXT NOT NULL, provider TEXT NOT NULL, state TEXT NOT NULL,
                     started_at INTEGER, finished_at INTEGER, failure_reason TEXT, PRIMARY KEY (task_id, id));
                 CREATE TABLE publications (idempotency_key TEXT PRIMARY KEY NOT NULL, repository TEXT NOT NULL,
                     branch TEXT NOT NULL, commit_sha TEXT, pull_request TEXT, phase TEXT NOT NULL);
                 INSERT INTO tasks VALUES ('task-1', 'legacy', 'developer', 'failed');
                 INSERT INTO attempts VALUES ('task-1', 'attempt-1', 'codex', 'failed', 10, 20, 'timeout');
                 INSERT INTO publications VALUES ('key-1', 'owner/repo', 'main', 'abc', '42', 'published');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        let ledger = SqliteExecutionLedger::from_connection(connection).unwrap();

        let attempt = ledger
            .get_attempt(&TaskId::new("task-1"), &AttemptId::new("attempt-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            attempt.attempt().failure_reason(),
            Some(&AttemptFailureReason::Timeout)
        );
        assert_eq!(attempt.attempt().requested_model(), None);
        assert_eq!(attempt.attempt().observed_provider(), None);
        assert_eq!(attempt.attempt().observed_model(), None);
        let publication = ledger.load_publication("key-1").unwrap().unwrap();
        assert_eq!(publication.repository(), "owner/repo");
        assert_eq!(publication.branch(), "main");
        assert_eq!(publication.commit_sha(), Some("abc"));
        assert_eq!(publication.pull_request(), Some("42"));
        assert_eq!(publication.task_id(), None);
        assert_eq!(publication.workspace(), None);
        assert_eq!(publication.base(), None);
        assert_eq!(publication.title(), None);
        assert_eq!(publication.body(), None);
    }

    #[test]
    fn future_schema_version_is_rejected() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA user_version = 10;")
            .unwrap();
        assert!(matches!(
            SqliteExecutionLedger::from_connection(connection),
            Err(LedgerError::UnsupportedSchemaVersion(10))
        ));
    }
}
